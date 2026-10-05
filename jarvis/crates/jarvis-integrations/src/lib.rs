//! Integrationen von JARVIS. Jede Integration liefert ihre Tools mit
//! vollständiger [`ToolSpec`](jarvis_permissions::ToolSpec); registriert
//! und ausgeführt werden sie ausschließlich über den Gateway.

pub mod fs;
pub mod http;
pub mod mail;
pub mod memory;
pub mod oauth;
pub mod teams;
pub mod web;
pub mod webuntis;
