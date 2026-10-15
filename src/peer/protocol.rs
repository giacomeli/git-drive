//! Messages of the notification channel. They are hints, never data: a
//! receiver only decides whether to wake its pull watcher.

use serde::{Deserialize, Serialize};

/// Protocol name negotiated on every connection. A future incompatible
/// protocol gets a new ALPN instead of a new message shape.
pub const ALPN: &[u8] = b"git-drive/notify/1";
/// Messages are a branch name and an object id: anything bigger is not ours.
pub const MAX_MESSAGE_BYTES: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Message {
    /// Sent by both sides when a connection opens: covers whatever was
    /// pushed while they were apart.
    Hello { branch: String, head: String },
    /// Sent to every connected peer after a successful push.
    Pushed { branch: String, commit: String },
}

#[derive(Debug)]
pub enum ProtocolError {
    TooLarge(usize),
    Malformed(String),
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtocolError::TooLarge(n) => write!(f, "message of {} bytes over the {} limit", n, MAX_MESSAGE_BYTES),
            ProtocolError::Malformed(e) => write!(f, "malformed message: {}", e),
        }
    }
}

impl std::error::Error for ProtocolError {}

impl Message {
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a message always serializes")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, ProtocolError> {
        if bytes.len() > MAX_MESSAGE_BYTES {
            return Err(ProtocolError::TooLarge(bytes.len()));
        }
        let msg: Message =
            serde_json::from_slice(bytes).map_err(|e| ProtocolError::Malformed(e.to_string()))?;
        let oid = msg.commit();
        if oid.len() != 40 || !oid.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ProtocolError::Malformed(format!("not an object id: {:?}", oid)));
        }
        Ok(msg)
    }

    pub fn branch(&self) -> &str {
        match self {
            Message::Hello { branch, .. } | Message::Pushed { branch, .. } => branch,
        }
    }

    /// The commit the sender has: its HEAD (`Hello`) or what it pushed.
    pub fn commit(&self) -> &str {
        match self {
            Message::Hello { head, .. } => head,
            Message::Pushed { commit, .. } => commit,
        }
    }
}

/// Whether `msg` must wake the pull watcher: it is about our branch and names
/// a commit this repository does not have yet.
pub fn should_wake(msg: &Message, local_branch: &str, commit_known: impl FnOnce(git2::Oid) -> bool) -> bool {
    if msg.branch() != local_branch {
        return false;
    }
    match git2::Oid::from_str(msg.commit()) {
        Ok(oid) => !commit_known(oid),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OID: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn messages_round_trip() {
        for msg in [
            Message::Hello { branch: "main".into(), head: OID.into() },
            Message::Pushed { branch: "main".into(), commit: OID.into() },
        ] {
            assert_eq!(Message::decode(&msg.encode()).unwrap(), msg);
        }
    }

    #[test]
    fn decode_rejects_oversized_garbage_and_bad_commits() {
        let big = vec![b' '; MAX_MESSAGE_BYTES + 1];
        assert!(Message::decode(&big).is_err(), "over the size limit");
        assert!(Message::decode(b"not json").is_err());
        assert!(Message::decode(br#"{"type":"Unknown","branch":"main"}"#).is_err());
        let bad = br#"{"type":"Pushed","branch":"main","commit":"zz"}"#;
        assert!(Message::decode(bad).is_err(), "commit must be a full object id");
    }

    #[test]
    fn should_wake_only_for_an_unknown_commit_of_our_branch() {
        let pushed = |branch: &str| Message::Pushed { branch: branch.into(), commit: OID.into() };
        assert!(should_wake(&pushed("main"), "main", |_| false));
        assert!(!should_wake(&pushed("main"), "main", |_| true), "already have it");
        assert!(!should_wake(&pushed("other"), "main", |_| false), "other branch");
        let hello = Message::Hello { branch: "main".into(), head: OID.into() };
        assert!(should_wake(&hello, "main", |_| false), "a peer ahead of us on connect");
    }
}
