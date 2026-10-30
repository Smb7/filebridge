use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;

pub const MAX_COLS: u32 = 1000;
pub const MAX_ROWS: u32 = 500;
pub const MAX_INPUT: usize = 256 * 1024;
/// Base64 of `MAX_INPUT`, plus a little slack, before we refuse to decode.
pub const MAX_INPUT_ENCODED: usize = MAX_INPUT.saturating_mul(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizeError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputError {
    TooLarge,
    Invalid,
}

/// A login shell request. Columns and rows are protocol fields, not a command.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct LoginShell {
    pub cols: u32,
    pub rows: u32,
}

impl std::fmt::Debug for LoginShell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginShell")
            .field("cols", &self.cols)
            .field("rows", &self.rows)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PtySlot {
    pub term_id: String,
    pub session_id: String,
    pub closed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenDecision {
    Reuse(String),
    Create,
}

pub fn validate_size(cols: u32, rows: u32) -> Result<(), SizeError> {
    if cols == 0 || rows == 0 || cols > MAX_COLS || rows > MAX_ROWS {
        Err(SizeError)
    } else {
        Ok(())
    }
}

pub fn plan_login_shell(cols: u32, rows: u32) -> Result<LoginShell, SizeError> {
    validate_size(cols, rows)?;
    Ok(LoginShell { cols, rows })
}

pub fn validate_input(len: usize) -> Result<(), InputError> {
    if len > MAX_INPUT {
        Err(InputError::TooLarge)
    } else {
        Ok(())
    }
}

pub fn decode_input(data: &str) -> Result<Vec<u8>, InputError> {
    if data.len() > MAX_INPUT_ENCODED {
        return Err(InputError::TooLarge);
    }
    let bytes = BASE64.decode(data).map_err(|_| InputError::Invalid)?;
    validate_input(bytes.len())?;
    Ok(bytes)
}

pub fn encode_output(bytes: &[u8]) -> String {
    BASE64.encode(bytes)
}

/// One live PTY per session. A closed shell can be opened again.
pub fn decide_open(slots: &[PtySlot], session_id: &str) -> OpenDecision {
    slots
        .iter()
        .find(|slot| slot.session_id == session_id && !slot.closed)
        .map(|slot| OpenDecision::Reuse(slot.term_id.clone()))
        .unwrap_or(OpenDecision::Create)
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_COLS, MAX_INPUT, OpenDecision, PtySlot, decide_open, decode_input, encode_output,
        plan_login_shell, validate_input,
    };

    fn slot(term_id: &str, session_id: &str, closed: bool) -> PtySlot {
        PtySlot {
            term_id: term_id.to_string(),
            session_id: session_id.to_string(),
            closed,
        }
    }

    #[test]
    fn a_shell_is_a_login_shell_with_no_command_string() {
        let shell = plan_login_shell(80, 24).expect("size");
        let rendered = format!("{shell:?}");
        assert!(rendered.contains("80"));
        assert!(rendered.contains("24"));
        assert!(!rendered.contains("command"));
        assert!(!rendered.contains("exec"));
        assert!(!rendered.contains("sh -c"));
        assert!(plan_login_shell(0, 24).is_err());
        assert!(plan_login_shell(80, 0).is_err());
        assert!(plan_login_shell(MAX_COLS + 1, 24).is_err());
        assert!(plan_login_shell(80, super::MAX_ROWS + 1).is_err());
    }

    #[test]
    fn terminal_input_may_contain_nul_and_rejects_only_size() {
        assert!(validate_input(0).is_ok());
        assert!(validate_input(1).is_ok());
        assert!(validate_input(MAX_INPUT).is_ok());
        assert!(validate_input(MAX_INPUT + 1).is_err());
        let nul = encode_output(&[0, 3, 255]);
        assert_eq!(decode_input(&nul).expect("nul"), vec![0, 3, 255]);
    }

    #[test]
    fn bad_input_is_not_echoed() {
        let garbage = "hunter2-secret-not-base64!!!";
        let error = decode_input(garbage).expect_err("bad");
        let rendered = format!("{error:?}");
        assert!(!rendered.contains("hunter2"));
        assert!(!rendered.contains("secret"));
    }

    #[test]
    fn one_open_pty_per_session_and_a_closed_one_can_be_replaced() {
        let slots = vec![slot("t-a", "ses-a", false), slot("t-b", "ses-b", false)];
        assert_eq!(
            decide_open(&slots, "ses-a"),
            OpenDecision::Reuse("t-a".to_string())
        );
        assert_eq!(decide_open(&slots, "ses-c"), OpenDecision::Create);
        let closed = vec![slot("t-a", "ses-a", true)];
        assert_eq!(decide_open(&closed, "ses-a"), OpenDecision::Create);
    }

    #[test]
    fn closing_one_session_leaves_the_other_slot() {
        let slots = [slot("t-a", "ses-a", false), slot("t-b", "ses-b", false)];
        let left: Vec<_> = slots
            .iter()
            .filter(|slot| slot.session_id != "ses-a")
            .cloned()
            .collect();
        assert_eq!(left, vec![slot("t-b", "ses-b", false)]);
        assert_eq!(
            decide_open(&left, "ses-b"),
            OpenDecision::Reuse("t-b".to_string())
        );
    }

    #[test]
    fn encoded_output_round_trips_without_appearing_in_a_size_error() {
        let secret = b"hunter2-secret";
        let encoded = encode_output(secret);
        assert_eq!(decode_input(&encoded).expect("round"), secret);
        let huge = "A".repeat(super::MAX_INPUT_ENCODED + 1);
        let error = decode_input(&huge).expect_err("huge");
        assert!(!format!("{error:?}").contains("hunter2"));
    }
}
