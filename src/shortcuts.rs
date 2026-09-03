//! Keyboard decision logic shared by the GTK event controller and tests.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShortcutAction {
    CopySelection,
    Paste,
    SendControlC,
    SendControlV,
    None,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ShortcutInput {
    pub key: char,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub has_selection: bool,
}

/// Decide whether a key event belongs to the desktop shortcut layer or should
/// be sent through the terminal PTY.  Alt is intentionally excluded from the
/// Ctrl+C copy/interruption rule so Ctrl+Alt+C remains available to programs.
pub fn decide_shortcut(input: ShortcutInput) -> ShortcutAction {
    let key = input.key.to_ascii_lowercase();
    if !input.ctrl {
        return ShortcutAction::None;
    }

    match (key, input.alt, input.shift) {
        ('c', false, false) => {
            if input.has_selection {
                ShortcutAction::CopySelection
            } else {
                ShortcutAction::SendControlC
            }
        }
        ('v', true, false) => ShortcutAction::SendControlV,
        ('v', false, false) => ShortcutAction::Paste,
        _ => ShortcutAction::None,
    }
}

pub const CONTROL_C: u8 = 0x03;
pub const CONTROL_V: u8 = 0x16;

/// Maximum byte sequence accepted from one user-defined key mapping. This
/// keeps a damaged profile from feeding an unbounded allocation to a PTY.
pub const MAX_KEY_SEQUENCE_BYTES: usize = 4_096;

/// Parse a user-facing chord such as `Ctrl+Shift+Right` into the canonical
/// key and modifier fields stored in a profile. The final segment is always
/// the key; preceding segments must be recognized modifiers.
pub fn parse_key_chord(value: &str) -> Result<(String, Vec<String>), &'static str> {
    let mut parts = value
        .split('+')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    let key = parts.pop().ok_or("key chord is empty")?;
    if key.chars().count() > 64 {
        return Err("key name is too long");
    }
    let mut modifiers = Vec::with_capacity(parts.len());
    for modifier in parts {
        let canonical = match modifier.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => "Ctrl",
            "alt" | "option" => "Alt",
            "shift" => "Shift",
            "meta" | "super" | "command" | "cmd" => "Meta",
            _ => return Err("unknown key modifier"),
        };
        if !modifiers.contains(&canonical.to_owned()) {
            modifiers.push(canonical.to_owned());
        }
    }
    Ok((key.to_owned(), modifiers))
}

/// Decode the notation used by Terminal-style key mappings. Plain UTF-8 is
/// preserved; `\\e`, `\\033`, `\\x1b`, `\\n`, `\\r`, `\\t`, and `\\\\` are
/// accepted escapes. The result is data for the active PTY, never a command.
pub fn decode_key_sequence(value: &str) -> Result<Vec<u8>, &'static str> {
    let mut output = Vec::with_capacity(value.len().min(MAX_KEY_SEQUENCE_BYTES));
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        if character != '\\' {
            let mut encoded = [0_u8; 4];
            output.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
        } else {
            let escape = characters.next().ok_or("trailing backslash")?;
            match escape {
                'e' | 'E' => output.push(0x1b),
                'n' => output.push(b'\n'),
                'r' => output.push(b'\r'),
                't' => output.push(b'\t'),
                '\\' => output.push(b'\\'),
                'x' => {
                    let high = characters.next().and_then(|value| value.to_digit(16));
                    let low = characters.next().and_then(|value| value.to_digit(16));
                    let (Some(high), Some(low)) = (high, low) else {
                        return Err("hex escape requires two digits");
                    };
                    output.push(((high << 4) | low) as u8);
                }
                '0'..='7' => {
                    let mut value = escape.to_digit(8).expect("matched octal digit");
                    for _ in 0..2 {
                        let Some(next) = characters.peek().and_then(|value| value.to_digit(8))
                        else {
                            break;
                        };
                        characters.next();
                        value = (value << 3) | next;
                    }
                    if value > u8::MAX as u32 {
                        return Err("octal escape exceeds one byte");
                    }
                    output.push(value as u8);
                }
                _ => return Err("unsupported escape"),
            }
        }
        if output.len() > MAX_KEY_SEQUENCE_BYTES {
            return Err("key sequence is too long");
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(key: char) -> ShortcutInput {
        ShortcutInput {
            key,
            ctrl: true,
            ..ShortcutInput::default()
        }
    }

    #[test]
    fn ctrl_c_copies_only_when_selection_exists() {
        let mut selected = input('c');
        selected.has_selection = true;
        assert_eq!(decide_shortcut(selected), ShortcutAction::CopySelection);
        assert_eq!(decide_shortcut(input('c')), ShortcutAction::SendControlC);
    }

    #[test]
    fn ctrl_v_pastes_without_shift() {
        assert_eq!(decide_shortcut(input('v')), ShortcutAction::Paste);
        assert_eq!(
            decide_shortcut(ShortcutInput {
                shift: true,
                ..input('v')
            }),
            ShortcutAction::None
        );
    }

    #[test]
    fn ctrl_alt_v_sends_literal_control_v() {
        assert_eq!(
            decide_shortcut(ShortcutInput {
                alt: true,
                ..input('v')
            }),
            ShortcutAction::SendControlV
        );
        assert_eq!(CONTROL_V, 0x16);
    }

    #[test]
    fn unrelated_or_shifted_shortcuts_are_left_to_the_terminal() {
        assert_eq!(
            decide_shortcut(ShortcutInput {
                key: 'x',
                ctrl: true,
                ..ShortcutInput::default()
            }),
            ShortcutAction::None
        );
        assert_eq!(
            decide_shortcut(ShortcutInput {
                key: 'c',
                ctrl: true,
                shift: true,
                ..ShortcutInput::default()
            }),
            ShortcutAction::None
        );
    }

    #[test]
    fn terminal_key_sequence_notation_decodes_to_bytes() {
        assert_eq!(
            decode_key_sequence(r"\eOP\033[15~\x03\r").unwrap(),
            [0x1b, b'O', b'P', 0x1b, b'[', b'1', b'5', b'~', 0x03, b'\r']
        );
        assert_eq!(decode_key_sequence("λ").unwrap(), "λ".as_bytes());
    }

    #[test]
    fn key_chords_are_canonical_and_bounded() {
        assert_eq!(
            parse_key_chord("Control+Shift+Right").unwrap(),
            (
                "Right".to_owned(),
                vec!["Ctrl".to_owned(), "Shift".to_owned()]
            )
        );
        assert_eq!(
            parse_key_chord("Option+v").unwrap(),
            ("v".to_owned(), vec!["Alt".to_owned()])
        );
        assert!(parse_key_chord("Hyper+F1").is_err());
        assert!(parse_key_chord("").is_err());
    }

    #[test]
    fn malformed_or_oversized_key_sequences_are_rejected() {
        assert!(decode_key_sequence(r"\x0").is_err());
        assert!(decode_key_sequence(r"\q").is_err());
        assert!(decode_key_sequence(&"x".repeat(MAX_KEY_SEQUENCE_BYTES + 1)).is_err());
    }
}
