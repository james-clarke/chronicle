//! Screen lock edges from logind (m32 chunk 0): the `Lock`/`Unlock` signals
//! a locker or `loginctl lock-session` raises on the display session, plus
//! its `LockedHint` property for lockers that set it. System bus.

use std::collections::HashMap;

use zbus::MatchRule;
use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::message::Type;
use zbus::zvariant::{OwnedObjectPath, Value};

use crate::{BoxError, LockSignal};

const LOGIN1: &str = "org.freedesktop.login1";
const SESSION_IFACE: &str = "org.freedesktop.login1.Session";

pub struct LogindLock {
    conn: Connection,
    session: OwnedObjectPath,
}

impl LogindLock {
    /// Fails without a system bus or a display session for this user.
    pub fn new() -> Result<Self, BoxError> {
        let conn = Connection::system()?;
        // The daemon runs under the user service, outside any session scope,
        // so `session/auto` would not resolve: the user's display session is
        // the one that locks.
        let user = Proxy::new(
            &conn,
            LOGIN1,
            "/org/freedesktop/login1/user/self",
            "org.freedesktop.login1.User",
        )?;
        let (_, session): (String, OwnedObjectPath) = user.get_property("Display")?;
        Ok(Self { conn, session })
    }

    fn locked_hint(&self) -> Result<bool, BoxError> {
        let session = Proxy::new(&self.conn, LOGIN1, self.session.clone(), SESSION_IFACE)?;
        Ok(session.get_property::<bool>("LockedHint")?)
    }
}

impl LockSignal for LogindLock {
    fn run(self, on_change: &mut dyn FnMut(bool)) -> Result<(), BoxError> {
        // Path only: logind is the sole owner of the session object, and a
        // well-known sender name would not match client-side.
        let rule = MatchRule::builder()
            .msg_type(Type::Signal)
            .path(self.session.clone())?
            .build();
        let messages = MessageIterator::for_match_rule(rule, &self.conn, None)?;
        on_change(self.locked_hint()?);
        for msg in messages {
            let msg = msg?;
            let header = msg.header();
            let Some(member) = header.member() else {
                continue;
            };
            match member.as_str() {
                "Lock" => on_change(true),
                "Unlock" => on_change(false),
                "PropertiesChanged" => {
                    let body = msg.body();
                    let (iface, changed, _): (String, HashMap<String, Value<'_>>, Vec<String>) =
                        body.deserialize()?;
                    if iface == SESSION_IFACE
                        && let Some(hint) = changed
                            .get("LockedHint")
                            .and_then(|v| bool::try_from(v.clone()).ok())
                    {
                        on_change(hint);
                    }
                }
                _ => {}
            }
        }
        Err("logind signal stream ended".into())
    }
}
