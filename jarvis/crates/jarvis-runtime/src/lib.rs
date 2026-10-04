//! Laufzeit-Kern: Service-Lifecycle, Tool-Registry und der zentrale
//! Tool-Gateway, durch den **jeder** Tool-Aufruf läuft.

pub mod gateway;
pub mod lifecycle;
pub mod tool;

pub use gateway::{AuditEvent, AuditSink, Confirmer, GatewayError, ToolGateway};
pub use lifecycle::{Service, ServiceManager, ServiceState};
pub use tool::{Tool, ToolError, ToolOutput, ToolRegistry};
