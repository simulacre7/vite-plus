//! Pure helpers over UTF-16 code units and bytes, shared by the Windows
//! implementation. They live outside win.rs so the unit tests run on every
//! platform.

const SPACE: u16 = b' ' as u16;
const TAB: u16 = b'\t' as u16;
const QUOTE: u16 = b'"' as u16;
const DOT: u16 = b'.' as u16;

/// Index where the raw command line's first (program) argument ends.
///
/// This follows the MSVC parsing rule for the program name: a quote toggles
/// quoted mode and backslashes have no escaping effect. The remainder
/// (`&cmdline[result..]`, leading whitespace included) is the argument tail to
/// forward to the child verbatim.
pub fn skip_program_argument(cmdline: &[u16]) -> usize {
    let mut i = 0;
    while i < cmdline.len() && (cmdline[i] == SPACE || cmdline[i] == TAB) {
        i += 1;
    }
    let mut quoted = false;
    while i < cmdline.len() {
        let c = cmdline[i];
        if c == QUOTE {
            quoted = !quoted;
        } else if (c == SPACE || c == TAB) && !quoted {
            break;
        }
        i += 1;
    }
    i
}

/// Length of the file stem, matching `Path::file_stem`: everything before the
/// last `.`, except that a leading `.` never starts an extension.
pub fn file_stem_len(name: &[u16]) -> usize {
    match name.iter().skip(1).rposition(|&c| c == DOT) {
        Some(pos) => pos + 1,
        None => name.len(),
    }
}

/// Case-sensitive comparison of a UTF-16 slice against an ASCII string.
pub fn eq_ascii(wide: &[u16], ascii: &[u8]) -> bool {
    wide.len() == ascii.len() && wide.iter().zip(ascii).all(|(&w, &a)| w == u16::from(a))
}

/// Format `value` as decimal ASCII into `buf`, returning the used suffix.
pub fn format_u32(mut value: u32, buf: &mut [u8; 10]) -> &[u8] {
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    &buf[i..]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn skips_unquoted_program() {
        let cl = wide(r"C:\bin\node.exe --version");
        assert_eq!(&cl[skip_program_argument(&cl)..], &wide(" --version")[..]);
    }

    #[test]
    fn skips_quoted_program_with_spaces() {
        let cl = wide(r#""C:\Program Files\node.exe" -e "1 + 1""#);
        assert_eq!(&cl[skip_program_argument(&cl)..], &wide(r#" -e "1 + 1""#)[..]);
    }

    #[test]
    fn skips_leading_whitespace_and_bare_program() {
        let cl = wide("  node");
        assert_eq!(skip_program_argument(&cl), cl.len());
        assert_eq!(skip_program_argument(&[]), 0);
    }

    #[test]
    fn keeps_argument_tail_verbatim() {
        let cl = wide(r#"npx "a  b\" literal" --flag"#);
        assert_eq!(&cl[skip_program_argument(&cl)..], &wide(r#" "a  b\" literal" --flag"#)[..]);
    }

    #[test]
    fn file_stem_matches_path_file_stem() {
        assert_eq!(file_stem_len(&wide("node.exe")), 4);
        assert_eq!(file_stem_len(&wide("node")), 4);
        assert_eq!(file_stem_len(&wide("NODE.EXE")), 4);
        assert_eq!(file_stem_len(&wide("a.b.exe")), 3);
        assert_eq!(file_stem_len(&wide(".hidden")), 7);
        assert_eq!(file_stem_len(&wide("node.")), 4);
    }

    #[test]
    fn eq_ascii_is_exact() {
        assert!(eq_ascii(&wide("vp"), b"vp"));
        assert!(!eq_ascii(&wide("VP"), b"vp"));
        assert!(!eq_ascii(&wide("vpx"), b"vp"));
    }

    #[test]
    fn formats_decimal() {
        let mut buf = [0u8; 10];
        assert_eq!(format_u32(0, &mut buf), b"0");
        let mut buf = [0u8; 10];
        assert_eq!(format_u32(203, &mut buf), b"203");
        let mut buf = [0u8; 10];
        assert_eq!(format_u32(u32::MAX, &mut buf), b"4294967295");
    }
}
