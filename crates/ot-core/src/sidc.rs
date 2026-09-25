//! Symbol identification codes: MIL-STD-2525C, MIL-STD-2525D or a CoT type.
//!
//! Every published track carries one, with its standard named, so consumers
//! can draw the right symbol without guessing the format. Feeds may supply
//! any of the three; the standard is recognised from the code's shape:
//!
//! * **2525C**: 15 characters, coding scheme first (`S`, `G`, `W`, `I`, `O`
//!   or `E`), e.g. `SFSPCLDD-------`. Position 2 is the standard identity,
//!   position 3 the battle dimension. Shorter codes are padded with `-`.
//! * **2525D**: 20 digits, e.g. `10033000001211000000`. Position 3 is the
//!   context (1 = exercise), 4 the standard identity, 5-6 the symbol set.
//! * **CoT**: a Cursor-on-Target type such as `a-f-S-C-L`.

use serde::{Deserialize, Serialize};

use crate::schema::{Affiliation, Domain};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SidcStandard {
    #[serde(rename = "2525c")]
    Mil2525C,
    #[serde(rename = "2525d")]
    Mil2525D,
    #[serde(rename = "cot")]
    Cot,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Sidc {
    pub standard: SidcStandard,
    pub code: String,
}

fn is_cot(s: &str) -> bool {
    let mut parts = s.split('-');
    let first = parts.next().unwrap_or_default();
    first.len() == 1
        && first.as_bytes()[0].is_ascii_lowercase()
        && parts.all(|p| {
            !p.is_empty()
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.')
        })
}

impl Sidc {
    /// Recognise and normalise a code, or `None` if it is none of the three.
    pub fn parse(raw: &str) -> Option<Sidc> {
        let s = raw.trim();
        if s.len() == 20 && s.bytes().all(|b| b.is_ascii_digit()) {
            return Some(Sidc {
                standard: SidcStandard::Mil2525D,
                code: s.to_owned(),
            });
        }
        let upper = s.to_ascii_uppercase().replace('*', "-");
        if (10..=15).contains(&upper.len())
            && matches!(upper.as_bytes()[0], b'S' | b'G' | b'W' | b'I' | b'O' | b'E')
            && upper
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-')
        {
            return Some(Sidc {
                standard: SidcStandard::Mil2525C,
                code: format!("{upper:-<15}"),
            });
        }
        is_cot(s).then(|| Sidc {
            standard: SidcStandard::Cot,
            code: s.to_owned(),
        })
    }

    fn char_at(&self, i: usize) -> Option<char> {
        self.code.as_bytes().get(i).map(|b| *b as char)
    }

    /// The standard identity the code carries.
    pub fn affiliation(&self) -> Option<Affiliation> {
        match self.standard {
            SidcStandard::Mil2525C => {
                Affiliation::from_cot_atom(&self.char_at(1)?.to_ascii_lowercase().to_string())
            }
            SidcStandard::Mil2525D => {
                let exercise = self.char_at(2)? == '1';
                Some(match self.char_at(3)? {
                    '0' => Affiliation::Pending,
                    '1' => Affiliation::Unknown,
                    '2' => Affiliation::AssumedFriend,
                    '3' => Affiliation::Friend,
                    '4' => Affiliation::Neutral,
                    '5' if exercise => Affiliation::Joker,
                    '5' => Affiliation::Suspect,
                    '6' if exercise => Affiliation::Faker,
                    '6' => Affiliation::Hostile,
                    _ => return None,
                })
            }
            SidcStandard::Cot => {
                let mut parts = self.code.split('-');
                (parts.next()? == "a")
                    .then(|| parts.next())
                    .flatten()
                    .and_then(Affiliation::from_cot_atom)
            }
        }
    }

    /// The domain the code's battle dimension or symbol set implies.
    pub fn domain(&self) -> Option<Domain> {
        match self.standard {
            SidcStandard::Mil2525C => match self.char_at(2)? {
                'P' => Some(Domain::Space),
                'A' => Some(Domain::Air),
                'G' | 'F' => Some(Domain::Ground),
                'S' => Some(Domain::Surface),
                'U' => Some(Domain::Subsurface),
                _ => None,
            },
            SidcStandard::Mil2525D => match self.code.get(4..6)? {
                "01" | "02" | "51" => Some(Domain::Air),
                "05" | "06" | "50" => Some(Domain::Space),
                "10" | "11" | "15" | "20" | "27" | "52" => Some(Domain::Ground),
                "30" | "53" => Some(Domain::Surface),
                "35" | "36" | "54" => Some(Domain::Subsurface),
                _ => None,
            },
            SidcStandard::Cot => {
                let mut parts = self.code.split('-');
                (parts.next()? == "a")
                    .then(|| parts.nth(1))
                    .flatten()
                    .and_then(Domain::from_cot_atom)
            }
        }
    }

    /// The same code with its standard identity replaced.
    pub fn with_affiliation(&self, a: Affiliation) -> Sidc {
        let mut bytes = self.code.clone().into_bytes();
        match self.standard {
            SidcStandard::Mil2525C if bytes.len() > 1 => {
                bytes[1] = a.cot_atom().to_ascii_uppercase() as u8;
            }
            SidcStandard::Mil2525D if bytes.len() > 3 => {
                let (context, identity) = match a {
                    Affiliation::Pending => (None, b'0'),
                    Affiliation::Unknown | Affiliation::None => (None, b'1'),
                    Affiliation::AssumedFriend => (None, b'2'),
                    Affiliation::Friend => (None, b'3'),
                    Affiliation::Neutral => (None, b'4'),
                    Affiliation::Suspect => (None, b'5'),
                    Affiliation::Hostile => (None, b'6'),
                    Affiliation::Joker => (Some(b'1'), b'5'),
                    Affiliation::Faker => (Some(b'1'), b'6'),
                };
                if let Some(c) = context {
                    bytes[2] = c;
                }
                bytes[3] = identity;
            }
            SidcStandard::Cot => {
                let mut parts: Vec<String> = self.code.split('-').map(str::to_owned).collect();
                if parts.len() >= 2 && parts[0] == "a" {
                    parts[1] = a.cot_atom().to_string();
                }
                return Sidc {
                    standard: self.standard,
                    code: parts.join("-"),
                };
            }
            _ => {}
        }
        Sidc {
            standard: self.standard,
            code: String::from_utf8(bytes).unwrap_or_else(|_| self.code.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_the_three_standards() {
        let c = Sidc::parse("sfspcldd-------").unwrap();
        assert_eq!(
            (c.standard, c.code.as_str()),
            (SidcStandard::Mil2525C, "SFSPCLDD-------")
        );
        assert_eq!(Sidc::parse("SHAP**----").unwrap().code, "SHAP-----------");
        let d = Sidc::parse("10033000001211000000").unwrap();
        assert_eq!(d.standard, SidcStandard::Mil2525D);
        let cot = Sidc::parse("a-f-S-C-L").unwrap();
        assert_eq!(cot.standard, SidcStandard::Cot);
        for bad in ["", "hello world", "123", "XFSP-----------", "a--f", "A-f-S"] {
            assert_eq!(Sidc::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn reads_identity_and_domain() {
        let c = Sidc::parse("SHSPCLDD-------").unwrap();
        assert_eq!(
            (c.affiliation(), c.domain()),
            (Some(Affiliation::Hostile), Some(Domain::Surface))
        );
        let air = Sidc::parse("SFAPMF---------").unwrap();
        assert_eq!(air.domain(), Some(Domain::Air));
        // 2525D: context 0, identity 3 (friend), symbol set 30 (sea surface).
        let d = Sidc::parse("10033000001211000000").unwrap();
        assert_eq!(
            (d.affiliation(), d.domain()),
            (Some(Affiliation::Friend), Some(Domain::Surface))
        );
        let joker = Sidc::parse("10150100001101000000").unwrap();
        assert_eq!(
            (joker.affiliation(), joker.domain()),
            (Some(Affiliation::Joker), Some(Domain::Air))
        );
        let cot = Sidc::parse("a-n-U").unwrap();
        assert_eq!(
            (cot.affiliation(), cot.domain()),
            (Some(Affiliation::Neutral), Some(Domain::Subsurface))
        );
    }

    #[test]
    fn affiliation_overrides_rewrite_the_identity() {
        let c = Sidc::parse("SUSPCLDD-------")
            .unwrap()
            .with_affiliation(Affiliation::Friend);
        assert_eq!(c.code, "SFSPCLDD-------");
        let d = Sidc::parse("10013000001211000000")
            .unwrap()
            .with_affiliation(Affiliation::Hostile);
        assert_eq!(d.code, "10063000001211000000");
        let f = Sidc::parse("10013000001211000000")
            .unwrap()
            .with_affiliation(Affiliation::Faker);
        assert_eq!(f.code, "10163000001211000000");
        assert_eq!(f.affiliation(), Some(Affiliation::Faker));
        let cot = Sidc::parse("a-u-S-X")
            .unwrap()
            .with_affiliation(Affiliation::Neutral);
        assert_eq!(cot.code, "a-n-S-X");
    }
}
