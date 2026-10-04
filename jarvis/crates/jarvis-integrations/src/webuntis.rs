//! WebUntis: **nur lesen** (Stundenplan, Ferien, Schuljahr).
//!
//! WebUntis nutzt JSON-RPC (immer POST). Deshalb ist die Absicherung hier
//! eine geschlossene Aufzählung [`UntisMethod`]: Der Client kann technisch
//! nur diese Lese-Methoden aufrufen; einen Konstruktor aus beliebigen
//! Strings gibt es nicht.

use crate::http::{HttpError, USER_AGENT};
use async_trait::async_trait;
use chrono::{Datelike, Duration as ChronoDuration, Local, NaiveDate};
use jarvis_permissions::{Access, CallFacts, Capability, Confirmation, Integration, RiskLevel, ToolSpec};
use jarvis_runtime::{Tool, ToolError, ToolOutput};
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

/// Die einzigen JSON-RPC-Methoden, die JARVIS kennt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UntisMethod {
    Authenticate,
    Logout,
    GetTimetable,
    GetHolidays,
    GetCurrentSchoolyear,
    GetSubjects,
    GetTimegridUnits,
}

impl UntisMethod {
    pub const fn as_str(self) -> &'static str {
        match self {
            UntisMethod::Authenticate => "authenticate",
            UntisMethod::Logout => "logout",
            UntisMethod::GetTimetable => "getTimetable",
            UntisMethod::GetHolidays => "getHolidays",
            UntisMethod::GetCurrentSchoolyear => "getCurrentSchoolyear",
            UntisMethod::GetSubjects => "getSubjects",
            UntisMethod::GetTimegridUnits => "getTimegridUnits",
        }
    }
    pub const ALL: [UntisMethod; 7] = [
        UntisMethod::Authenticate,
        UntisMethod::Logout,
        UntisMethod::GetTimetable,
        UntisMethod::GetHolidays,
        UntisMethod::GetCurrentSchoolyear,
        UntisMethod::GetSubjects,
        UntisMethod::GetTimegridUnits,
    ];
}

#[derive(Debug, Clone)]
pub struct UntisConfig {
    /// z. B. "https://mese.webuntis.com"
    pub server: String,
    pub school: String,
    pub username: String,
    /// Wird vom Host aus dem macOS-Schlüsselbund geladen.
    pub password: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Lesson {
    pub date: String,
    pub start: String,
    pub end: String,
    pub subject: String,
    pub teacher: String,
    pub room: String,
    pub status: String,
    pub info: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Holiday {
    pub name: String,
    pub start: String,
    pub end: String,
}

struct Session {
    id: String,
    person_id: i64,
    person_type: i64,
}

pub struct UntisClient {
    cfg: UntisConfig,
    http: reqwest::Client,
    session: Mutex<Option<Session>>,
}

fn fmt_date(d: i64) -> String {
    let s = d.to_string();
    if s.len() == 8 {
        format!("{}-{}-{}", &s[..4], &s[4..6], &s[6..])
    } else {
        s
    }
}

fn fmt_time(t: i64) -> String {
    format!("{:02}:{:02}", t / 100, t % 100)
}

fn names(v: &Value, key: &str) -> String {
    v[key]
        .as_array()
        .map(|a| a.iter().filter_map(|x| x["longname"].as_str().or(x["name"].as_str())).collect::<Vec<_>>().join(", "))
        .unwrap_or_default()
}

pub fn to_untis_date(d: NaiveDate) -> i64 {
    (d.year() as i64) * 10000 + (d.month() as i64) * 100 + d.day() as i64
}

impl UntisClient {
    pub fn new(cfg: UntisConfig) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(20))
            .build()
            .expect("HTTP-Client");
        Self { cfg, http, session: Mutex::new(None) }
    }

    fn endpoint(&self) -> String {
        let school: String = url::form_urlencoded::byte_serialize(self.cfg.school.as_bytes()).collect();
        format!("{}/WebUntis/jsonrpc.do?school={school}", self.cfg.server.trim_end_matches('/'))
    }

    async fn rpc(&self, method: UntisMethod, params: Value, session: Option<&str>) -> Result<Value, HttpError> {
        let mut req = self.http.post(self.endpoint()).json(&json!({
            "id": "jarvis", "method": method.as_str(), "params": params, "jsonrpc": "2.0"
        }));
        if let Some(s) = session {
            req = req.header("Cookie", format!("JSESSIONID={s}"));
        }
        let r = req.send().await.map_err(|e| HttpError::Network(e.to_string()))?;
        let status = r.status().as_u16();
        let v: Value = r.json().await.map_err(|e| HttpError::Network(e.to_string()))?;
        if status >= 400 {
            return Err(HttpError::Status { status, body: v.to_string() });
        }
        if let Some(e) = v.get("error").filter(|e| !e.is_null()) {
            return Err(HttpError::Status { status, body: e["message"].as_str().unwrap_or("WebUntis-Fehler").into() });
        }
        Ok(v["result"].clone())
    }

    async fn ensure_session(&self) -> Result<(), HttpError> {
        let mut s = self.session.lock().await;
        if s.is_some() {
            return Ok(());
        }
        let r = self
            .rpc(
                UntisMethod::Authenticate,
                json!({"user": self.cfg.username, "password": self.cfg.password, "client": "JARVIS"}),
                None,
            )
            .await
            .map_err(|e| HttpError::Auth(format!("WebUntis-Anmeldung fehlgeschlagen: {e}")))?;
        *s = Some(Session {
            id: r["sessionId"].as_str().ok_or_else(|| HttpError::Auth("keine Session".into()))?.into(),
            person_id: r["personId"].as_i64().unwrap_or(0),
            person_type: r["personType"].as_i64().unwrap_or(5),
        });
        Ok(())
    }

    async fn call(&self, method: UntisMethod, params: impl Fn(i64, i64) -> Value) -> Result<Value, HttpError> {
        self.ensure_session().await?;
        let (sid, pid, pty) = {
            let s = self.session.lock().await;
            let s = s.as_ref().unwrap();
            (s.id.clone(), s.person_id, s.person_type)
        };
        match self.rpc(method, params(pid, pty), Some(&sid)).await {
            Err(HttpError::Status { body, .. }) if body.to_lowercase().contains("not authenticated") => {
                *self.session.lock().await = None;
                self.ensure_session().await?;
                let s = self.session.lock().await;
                let s = s.as_ref().unwrap();
                self.rpc(method, params(s.person_id, s.person_type), Some(&s.id)).await
            }
            r => r,
        }
    }

    pub async fn timetable(&self, from: NaiveDate, to: NaiveDate) -> Result<Vec<Lesson>, HttpError> {
        let (f, t) = (to_untis_date(from), to_untis_date(to));
        let v = self
            .call(UntisMethod::GetTimetable, |pid, pty| {
                json!({"options": {
                    "element": {"id": pid, "type": pty},
                    "startDate": f, "endDate": t,
                    "showInfo": true, "showSubstText": true, "showLsText": true,
                    "subjectFields": ["name", "longname"], "teacherFields": ["name"], "roomFields": ["name"]
                }})
            })
            .await?;
        let mut out: Vec<Lesson> = v
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|p| Lesson {
                        date: fmt_date(p["date"].as_i64().unwrap_or(0)),
                        start: fmt_time(p["startTime"].as_i64().unwrap_or(0)),
                        end: fmt_time(p["endTime"].as_i64().unwrap_or(0)),
                        subject: names(p, "su"),
                        teacher: names(p, "te"),
                        room: names(p, "ro"),
                        status: p["code"].as_str().unwrap_or("regulär").into(),
                        info: [p["substText"].as_str(), p["info"].as_str(), p["lstext"].as_str()]
                            .into_iter()
                            .flatten()
                            .filter(|s| !s.is_empty())
                            .collect::<Vec<_>>()
                            .join(" · "),
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.sort_by(|a, b| (&a.date, &a.start).cmp(&(&b.date, &b.start)));
        Ok(out)
    }

    pub async fn holidays(&self) -> Result<Vec<Holiday>, HttpError> {
        let v = self.call(UntisMethod::GetHolidays, |_, _| json!({})).await?;
        Ok(v.as_array()
            .map(|a| {
                a.iter()
                    .map(|h| Holiday {
                        name: h["longName"].as_str().or(h["name"].as_str()).unwrap_or_default().into(),
                        start: fmt_date(h["startDate"].as_i64().unwrap_or(0)),
                        end: fmt_date(h["endDate"].as_i64().unwrap_or(0)),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    pub async fn logout(&self) {
        let s = self.session.lock().await.take();
        if let Some(s) = s {
            let _ = self.rpc(UntisMethod::Logout, json!({}), Some(&s.id)).await;
        }
    }
}

// ---------------- Tools ----------------

const CAPS: &[Capability] = &[Capability::WebUntisRead, Capability::NetworkGet];

macro_rules! spec {
    ($name:literal, $desc:literal) => {
        ToolSpec {
            name: $name,
            description: $desc,
            integration: Integration::WebUntis,
            access: Access::Read,
            risk: RiskLevel::Low,
            confirmation: Confirmation::Never,
            capabilities: CAPS,
            services: &[],
        }
    };
}

static TIMETABLE: ToolSpec = spec!("webuntis_timetable", "Stundenplan für einen Zeitraum (Standard: heute + 6 Tage), inkl. Entfall/Vertretung.");
static HOLIDAYS: ToolSpec = spec!("webuntis_holidays", "Ferien und schulfreie Tage.");
static SEARCH: ToolSpec = spec!("webuntis_search", "Sucht im Stundenplan (Fach, Lehrer, Raum, Info) und in Ferien.");

fn parse_date(v: Option<&Value>, default: NaiveDate) -> Result<NaiveDate, ToolError> {
    match v.and_then(Value::as_str) {
        None => Ok(default),
        Some(s) => NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|_| ToolError::InvalidArgs(format!("Datum '{s}' (erwartet JJJJ-MM-TT)"))),
    }
}

fn lessons_text(v: &[Lesson]) -> String {
    v.iter()
        .map(|l| {
            let st = if l.status == "regulär" { String::new() } else { format!(" [{}]", l.status) };
            let info = if l.info.is_empty() { String::new() } else { format!(" – {}", l.info) };
            format!("{} {}-{} {} ({}, {}){st}{info}", l.date, l.start, l.end, l.subject, l.teacher, l.room)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub struct UntisTool {
    client: Arc<UntisClient>,
    spec: &'static ToolSpec,
}

#[async_trait]
impl Tool for UntisTool {
    fn spec(&self) -> &ToolSpec {
        self.spec
    }
    fn parameters(&self) -> Value {
        match self.spec.name {
            "webuntis_timetable" => json!({"type":"object","properties":{"from":{"type":"string","description":"JJJJ-MM-TT"},"to":{"type":"string"}}}),
            "webuntis_search" => json!({"type":"object","properties":{"query":{"type":"string"},"days":{"type":"integer"}},"required":["query"]}),
            _ => json!({"type":"object","properties":{}}),
        }
    }
    fn facts(&self, _a: &Value) -> Result<CallFacts, ToolError> {
        Ok(CallFacts { preview: self.spec.name.into(), ..Default::default() })
    }
    async fn call(&self, a: Value) -> Result<ToolOutput, ToolError> {
        let e = |e: HttpError| ToolError::Failed(e.to_string());
        let today = Local::now().date_naive();
        match self.spec.name {
            "webuntis_timetable" => {
                let from = parse_date(a.get("from"), today)?;
                let to = parse_date(a.get("to"), from + ChronoDuration::days(6))?;
                if to < from || (to - from).num_days() > 31 {
                    return Err(ToolError::InvalidArgs("Zeitraum ungültig (max. 31 Tage)".into()));
                }
                let l = self.client.timetable(from, to).await.map_err(e)?;
                Ok(ToolOutput::with_data(lessons_text(&l), json!(l)))
            }
            "webuntis_holidays" => {
                let h = self.client.holidays().await.map_err(e)?;
                let t = h.iter().map(|h| format!("{}: {} bis {}", h.name, h.start, h.end)).collect::<Vec<_>>().join("\n");
                Ok(ToolOutput::with_data(t, json!(h)))
            }
            "webuntis_search" => {
                let q = a.get("query").and_then(Value::as_str).ok_or_else(|| ToolError::InvalidArgs("'query' fehlt".into()))?.to_lowercase();
                let days = a.get("days").and_then(Value::as_i64).unwrap_or(14).clamp(1, 31);
                let l = self.client.timetable(today, today + ChronoDuration::days(days)).await.map_err(e)?;
                let hits: Vec<Lesson> = l
                    .into_iter()
                    .filter(|l| [&l.subject, &l.teacher, &l.room, &l.info, &l.status].iter().any(|f| f.to_lowercase().contains(&q)))
                    .collect();
                let hol: Vec<Holiday> = self.client.holidays().await.unwrap_or_default().into_iter().filter(|h| h.name.to_lowercase().contains(&q)).collect();
                let mut t = lessons_text(&hits);
                for h in &hol {
                    t.push_str(&format!("\nFerien: {} ({} bis {})", h.name, h.start, h.end));
                }
                Ok(ToolOutput::with_data(t, json!({"lessons": hits, "holidays": hol})))
            }
            _ => Err(ToolError::Forbidden("unbekannte WebUntis-Operation".into())),
        }
    }
}

pub fn tools(client: Arc<UntisClient>) -> Vec<Arc<dyn Tool>> {
    [&TIMETABLE, &HOLIDAYS, &SEARCH]
        .into_iter()
        .map(|spec| Arc::new(UntisTool { client: client.clone(), spec }) as Arc<dyn Tool>)
        .collect()
}
