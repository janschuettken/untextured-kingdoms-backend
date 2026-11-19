//! E-Mails verschicken (bisher nur „Passwort vergessen“) über SMTP.
//!
//! Einstellungen aus der Umgebung (`.env`):
//! - `SMTP_HOST`      z. B. `smtp.example.org`. Fehlt er, wird nichts verschickt und der
//!   Link steht nur im Server-Log (praktisch zum lokalen Testen).
//! - `SMTP_PORT`      Standard 587 (STARTTLS); 465 = TLS von Anfang an
//! - `SMTP_USER`, `SMTP_PASSWORD`  Anmeldung beim Mailserver
//! - `SMTP_FROM`      Absender, z. B. `Untextured Kingdoms <spiel@example.org>`
//! - `PUBLIC_URL`     Adresse der Seite für Links, z. B. `https://untexturedkingdoms.example.org`

use lettre::{
    message::{header::ContentType, Mailbox},
    transport::smtp::authentication::Credentials,
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
};
use tracing::{info, warn};

#[derive(Clone)]
pub struct Mailer {
    smtp: Option<(AsyncSmtpTransport<Tokio1Executor>, Mailbox)>,
    /// Ohne abschließenden Schrägstrich. Leer = Link relativ (nur im Log sinnvoll).
    pub public_url: String,
}

impl Mailer {
    pub fn from_env() -> Mailer {
        let env = |k: &str| std::env::var(k).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let public_url = env("PUBLIC_URL").unwrap_or_default().trim_end_matches('/').to_string();
        let Some(host) = env("SMTP_HOST") else {
            info!("Kein SMTP_HOST: E-Mails werden nicht verschickt, Links stehen im Log");
            return Mailer { smtp: None, public_url };
        };
        let port: u16 = env("SMTP_PORT").and_then(|p| p.parse().ok()).unwrap_or(587);
        let from = env("SMTP_FROM").or_else(|| env("SMTP_USER")).unwrap_or_default();
        let Ok(from) = from.parse::<Mailbox>() else {
            warn!("SMTP_FROM ({from}) ist keine gültige Adresse: E-Mails sind aus");
            return Mailer { smtp: None, public_url };
        };
        let builder = if port == 465 {
            AsyncSmtpTransport::<Tokio1Executor>::relay(&host)
        } else {
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&host)
        };
        let mut builder = match builder {
            Ok(b) => b.port(port),
            Err(e) => {
                warn!("SMTP {host}: {e}: E-Mails sind aus");
                return Mailer { smtp: None, public_url };
            }
        };
        if let (Some(user), Some(pass)) = (env("SMTP_USER"), env("SMTP_PASSWORD")) {
            builder = builder.credentials(Credentials::new(user, pass));
        }
        info!("E-Mails über {host}:{port}");
        Mailer { smtp: Some((builder.build(), from)), public_url }
    }

    pub fn enabled(&self) -> bool {
        self.smtp.is_some()
    }

    /// Eine Text-Mail schicken. Ohne SMTP nur ins Log.
    pub async fn send(&self, to: &str, subject: &str, body: &str) -> Result<(), String> {
        let Some((transport, from)) = &self.smtp else {
            info!("(keine Mail, SMTP aus) An {to}: {subject}\n{body}");
            return Ok(());
        };
        let to: Mailbox = to.parse().map_err(|e| format!("Empfänger: {e}"))?;
        let msg = Message::builder()
            .from(from.clone())
            .to(to)
            .subject(subject)
            .header(ContentType::TEXT_PLAIN)
            .body(body.to_string())
            .map_err(|e| e.to_string())?;
        transport.send(msg).await.map(|_| ()).map_err(|e| e.to_string())
    }
}
