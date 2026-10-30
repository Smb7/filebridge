//! Editor window rules. No sockets, no file bytes on disk, and no shell strings.
//! A pane is not known here: the cap is per open file, not left versus right.

pub const MAX_EDITORS: usize = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenKey {
    pub connection_id: String,
    pub session_id: String,
    pub path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenPlan {
    Focus { index: usize },
    Create,
    Refuse,
}

/// An already-open connection+path is focused, including after the session id
/// changes on reconnect. A third distinct file is refused.
pub fn plan_open(open: &[OpenKey], incoming: &OpenKey) -> OpenPlan {
    if let Some(index) = open.iter().position(|slot| same_file(slot, incoming)) {
        return OpenPlan::Focus { index };
    }
    if open.len() >= MAX_EDITORS {
        return OpenPlan::Refuse;
    }
    OpenPlan::Create
}

fn same_file(slot: &OpenKey, incoming: &OpenKey) -> bool {
    if slot.path != incoming.path || slot.path.is_empty() {
        return false;
    }
    if !slot.connection_id.is_empty() && slot.connection_id == incoming.connection_id {
        return true;
    }
    !slot.session_id.is_empty() && slot.session_id == incoming.session_id
}

/// `{connection name} — {user}@{host}:{port} — {path}`. The OS title is set from
/// this string. It never includes a secret.
pub fn editor_title(name: &str, username: &str, host: &str, port: u16, path: &str) -> String {
    format!("{name} — {username}@{host}:{port} — {path}")
}

#[cfg(test)]
mod tests {
    use super::{MAX_EDITORS, OpenKey, OpenPlan, editor_title, plan_open};

    fn key(connection: &str, session: &str, path: &str) -> OpenKey {
        OpenKey {
            connection_id: connection.to_string(),
            session_id: session.to_string(),
            path: path.to_string(),
        }
    }

    #[test]
    fn the_cap_is_two_and_an_open_file_is_focused() {
        assert_eq!(MAX_EDITORS, 2);
        let first = key("conn-a", "ses-a", "/etc/telegraf/cpu.conf");
        assert_eq!(plan_open(&[], &first), OpenPlan::Create);
        assert_eq!(
            plan_open(std::slice::from_ref(&first), &first),
            OpenPlan::Focus { index: 0 }
        );

        let second = key("conn-b", "ses-b", "/etc/telegraf/mem.conf");
        let open = vec![first.clone(), second];
        assert_eq!(plan_open(&open, &first), OpenPlan::Focus { index: 0 });
        let third = key("conn-a", "ses-a", "/etc/telegraf/disk.conf");
        assert_eq!(plan_open(&open, &third), OpenPlan::Refuse);

        // Reconnect issues a new session id. The same server and path is the
        // same window, not a third editor.
        let rebound = key("conn-a", "ses-new", "/etc/telegraf/cpu.conf");
        assert_eq!(plan_open(&open, &rebound), OpenPlan::Focus { index: 0 });

        let same_session_other_path = key("conn-a", "ses-a", "/etc/telegraf/swap.conf");
        assert_eq!(
            plan_open(&[first], &same_session_other_path),
            OpenPlan::Create
        );
    }

    #[test]
    fn a_title_names_the_server_and_the_path_and_hides_secrets() {
        let title = editor_title(
            "prod-telegraf",
            "ubuntu",
            "10.0.0.8",
            22,
            "/etc/telegraf/telegraf.conf",
        );
        assert!(title.contains("prod-telegraf"));
        assert!(title.contains("ubuntu@10.0.0.8"));
        assert!(title.contains("/etc/telegraf/telegraf.conf"));
        assert!(title.contains('—'));
        assert!(!title.to_lowercase().contains("password"));
        assert!(!title.to_lowercase().contains("passphrase"));
        assert!(!title.contains("hunter2-secret"));
        assert!(!title.to_lowercase().contains("sudo"));
    }
}
