//! Password rules (Settings → Security → Passwords): length and character
//! classes when a password is set, how much a change must differ, and a
//! generator for passwords that meet them.

use super::settings::PasswordPolicy;

/// Longest password accepted (hashing is slow on purpose).
const MAX_LENGTH: usize = 256;

impl PasswordPolicy {
    /// Whether `p` may be set as a password; the error lists every rule it
    /// misses.
    pub fn check(&self, p: &str) -> Result<(), String> {
        super::check_password(p)?;
        let n = p.chars().count();
        if n > MAX_LENGTH {
            return Err(format!("a password has at most {MAX_LENGTH} characters"));
        }
        let mut missing = Vec::new();
        if n < self.min_length {
            missing.push(format!("at least {} characters", self.min_length));
        }
        let has = |f: fn(char) -> bool| p.chars().any(f);
        if self.require_upper && !has(char::is_uppercase) {
            missing.push("an upper-case letter".into());
        }
        if self.require_lower && !has(char::is_lowercase) {
            missing.push("a lower-case letter".into());
        }
        if self.require_digit && !has(|c| c.is_ascii_digit()) {
            missing.push("a digit".into());
        }
        if self.require_special && !has(|c| !c.is_alphanumeric()) {
            missing.push("a special character (not a letter or digit)".into());
        }
        if missing.is_empty() {
            Ok(())
        } else {
            Err(format!("the password needs {}", missing.join(", ")))
        }
    }

    /// Whether `new` differs enough from `current` (your own change).
    pub fn check_change(&self, current: &str, new: &str) -> Result<(), String> {
        if self.min_changed_chars > 0 && changed_chars(current, new) < self.min_changed_chars {
            return Err(format!(
                "change at least {} characters of the current password",
                self.min_changed_chars
            ));
        }
        Ok(())
    }
}

/// Characters to insert, delete or replace to turn `a` into `b`.
pub fn changed_chars(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut diag = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let next = if ca == cb {
                diag
            } else {
                1 + diag.min(row[j]).min(row[j + 1])
            };
            diag = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[b.len()]
}

/// A random password meeting any policy up to 26 characters: 80 bits of
/// randomness with every character class.
pub fn generate() -> String {
    // Random from the FIPS module's DRBG; the fixed parts give every class.
    let a = crate::fips::random_hex::<5>();
    let b = crate::fips::random_hex::<5>();
    format!("Ot-{a}-{}!7a", b.to_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_meet_the_policy() {
        let p = PasswordPolicy::default();
        assert!(p.check("Correct-Horse-7-Battery").is_ok());
        let e = p.check("correcthorsebattery").unwrap_err();
        assert!(
            e.contains("upper-case") && e.contains("digit") && e.contains("special"),
            "{e}"
        );
        assert!(p.check("Sh0rt!pass").unwrap_err().contains("15"));
        assert!(p.check("short").is_err(), "the floor always holds");
        assert!(p.check(&"Aa1!".repeat(100)).is_err(), "too long");
        let lax = PasswordPolicy {
            min_length: 8,
            require_upper: false,
            require_lower: false,
            require_digit: false,
            require_special: false,
            ..Default::default()
        };
        assert!(lax.check("abcdefgh").is_ok());
        assert!(p.check(&generate()).is_ok());
        assert_ne!(generate(), generate());
    }

    #[test]
    fn a_change_must_differ_enough() {
        let p = PasswordPolicy::default();
        assert_eq!(changed_chars("kitten", "sitting"), 3);
        assert_eq!(changed_chars("", "abc"), 3);
        assert!(
            p.check_change("Correct-Horse-7-Battery", "Correct-Horse-8-Battery")
                .is_err()
        );
        assert!(
            p.check_change("Correct-Horse-7-Battery", "Purple-Monkey-42-Dishwasher")
                .is_ok()
        );
    }
}
