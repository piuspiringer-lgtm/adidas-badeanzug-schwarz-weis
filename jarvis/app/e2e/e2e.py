"""E2E-Test der echten Desktop-App (Tauri + Rust-Agent) über tauri-driver.
Läuft unter Linux (CI); nutzt fake_ollama.py statt eines echten Modells."""
import os, sys, time
from pathlib import Path
from selenium import webdriver
from selenium.webdriver.common.by import By
from selenium.webdriver.common.options import ArgOptions
from selenium.webdriver.support.ui import WebDriverWait

APP, SHOTS = sys.argv[1], Path(sys.argv[2])
SHOTS.mkdir(parents=True, exist_ok=True)
target = Path.home() / "Documents" / "alt.txt"
target.parent.mkdir(parents=True, exist_ok=True)
target.write_text("alte Notiz")

opts = ArgOptions()
opts.set_capability("tauri:options", {"application": APP})
opts.set_capability("browserName", "wry")
d = webdriver.Remote(command_executor="http://127.0.0.1:4444", options=opts)
wait = WebDriverWait(d, 20)
d.set_window_size(1320, 840)

def send(text):
    box = wait.until(lambda d: d.find_element(By.CSS_SELECTOR, "textarea[aria-label='Nachricht an JARVIS']"))
    wait.until(lambda d: box.is_enabled() and not d.find_elements(By.CSS_SELECTOR, ".chat__input button[disabled]") or box.get_attribute("value") == "")
    box.send_keys(text)
    d.find_element(By.CSS_SELECTOR, ".chat__input button").click()

def last_answer(contains):
    return wait.until(lambda d: next((m.text for m in d.find_elements(By.CSS_SELECTOR, ".msg--jarvis p") if contains in m.text), None))

def check(cond, msg):
    print(("OK   " if cond else "FAIL ") + msg)
    if not cond:
        d.save_screenshot(str(SHOTS / "failure.png")); d.quit(); sys.exit(1)

try:
    try:
        wait.until(lambda d: d.find_element(By.CSS_SELECTOR, ".hero__text h1").text == "JARVIS")
    except Exception:
        d.save_screenshot(str(SHOTS / "failure.png"))
        raise
    wait.until(lambda d: "qwen3:8b" in d.find_element(By.CSS_SELECTOR, ".status").text)
    d.save_screenshot(str(SHOTS / "1-start.png"))
    check(True, "App gestartet, Status vom Rust-Kern geladen")

    send("Hallo JARVIS")
    check("Hallo! Ich bin JARVIS." in last_answer("Hallo"), "Smalltalk über den Agenten")

    send("Lösche alt.txt aus Dokumente")
    dlg = wait.until(lambda d: d.find_element(By.CSS_SELECTOR, "[role=alertdialog]"))
    check("fs_trash" in dlg.text and "alt.txt" in dlg.text, "Bestätigungsdialog für fs_trash erscheint")
    d.save_screenshot(str(SHOTS / "2-bestaetigung.png"))
    dlg.find_element(By.XPATH, ".//button[text()='Ablehnen']").click()
    ans = last_answer("nicht bestätigt")
    check(target.exists(), "abgelehnt → Datei existiert noch")
    check("nicht ausgeführt" in ans, "Antwort legt die Ablehnung offen")

    send("Lösche alt.txt jetzt wirklich")
    dlg = wait.until(lambda d: d.find_element(By.CSS_SELECTOR, "[role=alertdialog]"))
    dlg.find_element(By.XPATH, ".//button[text()='Ja, ausführen']").click()
    last_answer("Papierkorb")
    check(not target.exists(), "bestätigt → Datei im Papierkorb")
    check(any("Papierkorb" in e.text for e in d.find_elements(By.CSS_SELECTOR, ".act__verify.ok")), "Verifikation in der Aktivität sichtbar")

    send("Schick eine Mail an den Chef")
    ans = last_answer("send_email")
    check("blockiert" in ans, "send_email blockiert und offengelegt")
    d.save_screenshot(str(SHOTS / "3-chat.png"))

    d.find_element(By.XPATH, "//nav//button[text()='Protokoll']").click()
    rows = wait.until(lambda d: d.find_elements(By.CSS_SELECTOR, "tbody tr") or None)
    text = d.find_element(By.CSS_SELECTOR, ".page").text
    check("Hash-Kette intakt" in text, "Audit-Hash-Kette intakt")
    check("send_email" in text and "gesperrt" in text, "gesperrte Aktion im Protokoll")
    check("abgelehnt" in text and "bestätigt" in text, "Ablehnung und Bestätigung protokolliert")
    d.save_screenshot(str(SHOTS / "4-protokoll.png"))

    d.find_element(By.XPATH, "//nav//button[text()='Werkzeuge']").click()
    time.sleep(0.5)
    tools = d.find_element(By.CSS_SELECTOR, ".page").text
    check("fs_trash" in tools and "memory_recall" in tools, "Werkzeugliste vom Registry")
    d.save_screenshot(str(SHOTS / "5-werkzeuge.png"))
    print("ALLE E2E-PRÜFUNGEN BESTANDEN")
finally:
    d.quit()
