use std::fs::File;
use std::io::{self, Read};

#[derive(Clone, Debug)]
pub struct Credentials {
    pub pin: String,
    pub bootstrap: String,
    pub bearer: String,
}

impl Credentials {
    pub fn generate() -> io::Result<Self> {
        let mut random = [0_u8; 48];
        File::open("/dev/urandom")?.read_exact(&mut random)?;
        let number = u32::from_le_bytes(random[..4].try_into().unwrap()) % 1_000_000;
        Ok(Self {
            pin: format!("{number:06}"),
            bootstrap: hex(&random[4..20]),
            bearer: hex(&random[20..48]),
        })
    }

    pub fn authorizes_session(&self, pin: Option<&str>, bootstrap: Option<&str>) -> bool {
        pin.is_some_and(|value| constant_time_eq(value.as_bytes(), self.pin.as_bytes()))
            || bootstrap
                .is_some_and(|value| constant_time_eq(value.as_bytes(), self.bootstrap.as_bytes()))
    }

    pub fn authorizes_bearer(&self, value: Option<&str>) -> bool {
        value
            .and_then(|header| header.strip_prefix("Bearer "))
            .is_some_and(|token| constant_time_eq(token.as_bytes(), self.bearer.as_bytes()))
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_have_stable_shapes_and_separate_secrets() {
        let credentials = Credentials::generate().unwrap();
        assert_eq!(credentials.pin.len(), 6);
        assert!(credentials.pin.bytes().all(|byte| byte.is_ascii_digit()));
        assert_eq!(credentials.bootstrap.len(), 32);
        assert_eq!(credentials.bearer.len(), 56);
        assert_ne!(credentials.bootstrap, credentials.bearer);
        assert!(credentials.authorizes_session(Some(&credentials.pin), None));
        assert!(credentials.authorizes_bearer(Some(&format!("Bearer {}", credentials.bearer))));
    }
}
