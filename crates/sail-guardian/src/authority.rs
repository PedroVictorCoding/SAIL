//! Owner transport authentication. Event-bus source fields are not authority.
use ring::hmac;
#[derive(Clone)]
pub struct ApprovalAuthority {
    verifier: hmac::Tag,
}
impl ApprovalAuthority {
    pub fn new(token: &str) -> Result<Self, &'static str> {
        if token.len() < 32 {
            return Err("SAIL_APPROVAL_TOKEN must contain at least 32 bytes");
        }
        Ok(Self {
            verifier: hmac::sign(
                &hmac::Key::new(hmac::HMAC_SHA256, token.as_bytes()),
                b"SAIL-owner-approval-v1",
            ),
        })
    }
    pub fn authenticates(&self, candidate: &str) -> bool {
        candidate.len() >= 32
            && hmac::verify(
                &hmac::Key::new(hmac::HMAC_SHA256, candidate.as_bytes()),
                b"SAIL-owner-approval-v1",
                self.verifier.as_ref(),
            )
            .is_ok()
    }
}
