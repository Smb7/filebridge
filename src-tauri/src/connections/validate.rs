use super::{AuthMethod, ConnectionDraft, ValidatedConnection};

#[derive(Debug, PartialEq, Eq)]
pub struct ValidationError {
    pub message: String,
}

pub fn validate(draft: ConnectionDraft) -> Result<ValidatedConnection, ValidationError> {
    let name = required_text("Name", &draft.name, 80)?;
    let host = required_token("Host", &draft.host, 253)?;
    let username = required_token("Username", &draft.username, 64)?;
    if draft.port == 0 {
        return Err(invalid("Port must be between 1 and 65535."));
    }

    let key_path = optional_text("Key file path", draft.key_path, 1024)?;
    let default_path = optional_text("Default path", draft.default_path, 1024)?;
    let notes = optional_text("Notes", draft.notes, 2000)?;

    match draft.auth_method {
        AuthMethod::KeyFile if key_path.is_none() => {
            return Err(invalid(
                "A key file path is required for key file authentication.",
            ));
        }
        AuthMethod::KeyFile => {}
        _ if key_path.is_some() => {
            return Err(invalid(
                "A key file path is only used for key file authentication.",
            ));
        }
        _ => {}
    }

    let id = draft.id.and_then(|id| {
        let trimmed = id.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    });

    Ok(ValidatedConnection {
        id,
        name,
        host,
        port: draft.port,
        username,
        auth_method: draft.auth_method,
        key_path,
        default_path,
        notes,
    })
}

fn required_text(label: &str, value: &str, max: usize) -> Result<String, ValidationError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(invalid(format!("{label} is required.")));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(invalid(format!(
            "{label} cannot contain control characters."
        )));
    }
    if trimmed.chars().count() > max {
        return Err(invalid(format!(
            "{label} must be {max} characters or fewer."
        )));
    }
    Ok(trimmed.to_string())
}

fn required_token(label: &str, value: &str, max: usize) -> Result<String, ValidationError> {
    let trimmed = required_text(label, value, max)?;
    if trimmed.chars().any(char::is_whitespace) {
        return Err(invalid(format!("{label} cannot contain spaces.")));
    }
    Ok(trimmed)
}

fn optional_text(
    label: &str,
    value: Option<String>,
    max: usize,
) -> Result<Option<String>, ValidationError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(required_text(label, &value, max)?))
}

fn invalid(message: impl Into<String>) -> ValidationError {
    ValidationError {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::validate;
    use crate::connections::{AuthMethod, ConnectionDraft};

    fn draft() -> ConnectionDraft {
        ConnectionDraft {
            id: None,
            name: "prod".to_string(),
            host: "10.0.0.8".to_string(),
            port: 22,
            username: "ubuntu".to_string(),
            auth_method: AuthMethod::Password,
            key_path: None,
            default_path: Some(" /etc/telegraf ".to_string()),
            notes: Some("  ".to_string()),
        }
    }

    #[test]
    fn accepts_a_trimmed_connection() {
        let validated = validate(draft()).expect("valid");
        assert_eq!(validated.name, "prod");
        assert_eq!(validated.host, "10.0.0.8");
        assert_eq!(validated.default_path.as_deref(), Some("/etc/telegraf"));
        assert_eq!(validated.notes, None);
    }

    #[test]
    fn rejects_empty_host_port_zero_and_spaced_username() {
        let mut missing_host = draft();
        missing_host.host = "  ".to_string();
        assert!(validate(missing_host).unwrap_err().message.contains("Host"));

        let mut bad_port = draft();
        bad_port.port = 0;
        assert!(validate(bad_port).unwrap_err().message.contains("Port"));

        let mut spaced = draft();
        spaced.username = "ubuntu user".to_string();
        assert!(validate(spaced).unwrap_err().message.contains("Username"));
    }

    #[test]
    fn rejects_control_characters_in_the_name() {
        let mut named = draft();
        named.name = "prod\ntelegraf".to_string();
        assert!(validate(named).unwrap_err().message.contains("control"));
    }

    #[test]
    fn key_path_is_required_only_for_key_file() {
        let mut missing = draft();
        missing.auth_method = AuthMethod::KeyFile;
        assert!(validate(missing).unwrap_err().message.contains("key file"));

        let mut extra = draft();
        extra.key_path = Some("/home/ubuntu/.ssh/id_ed25519".to_string());
        assert!(validate(extra).unwrap_err().message.contains("only used"));

        let mut stored = draft();
        stored.auth_method = AuthMethod::KeyFile;
        stored.key_path = Some(" /home/ubuntu/.ssh/id_ed25519 ".to_string());
        let validated = validate(stored).expect("key file");
        assert_eq!(
            validated.key_path.as_deref(),
            Some("/home/ubuntu/.ssh/id_ed25519")
        );
    }
}
