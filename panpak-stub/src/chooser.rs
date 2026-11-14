// SPDX-License-Identifier: MIT OR Apache-2.0
//! The actor's side of the file dialog (docs/FILE-CHOOSER.md): the router sends the call of the app in a frame; the actor parses it again, shows the
//! dialog of the desktop, shares what the user chose and answers with a frame that carries the token of the request. One dialog at a time per copy and
//! at most five a minute; the work is on a thread of its own because the user takes as long as they take.

use panpak_core::chooserhost::{self, HostCtx, Outcome};
use panpak_core::dbus::Value;
use panpak_core::filechooser::{self, Call};
use panpak_core::frames;
use std::{
    collections::VecDeque,
    io::Write,
    os::unix::net::UnixStream,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

/// Dialogs a copy may open in a minute.
const PER_MINUTE: usize = 5;

type Runner = dyn Fn(&HostCtx, &Call) -> Outcome + Send + Sync;
type Writer = Arc<Mutex<UnixStream>>;

struct Gate {
    busy: AtomicBool,
    recent: Mutex<VecDeque<Instant>>,
}

/// Lets a dialog go only when none is open and fewer than five were opened in the last minute. Dropping the permit frees the copy.
struct Permit(Arc<Gate>);

impl Drop for Permit {
    fn drop(&mut self) {
        self.0.busy.store(false, Ordering::SeqCst);
    }
}

impl Gate {
    fn try_enter(self: &Arc<Self>, now: Instant) -> Result<Permit, &'static str> {
        if self.busy.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
            return Err("a file dialog is already open");
        }
        let permit = Permit(self.clone());
        let mut r = self.recent.lock().map_err(|_| "internal error")?;
        while r.front().is_some_and(|t| now.duration_since(*t) > Duration::from_secs(60)) {
            r.pop_front();
        }
        if r.len() >= PER_MINUTE {
            return Err("too many file dialogs in a minute");
        }
        r.push_back(now);
        Ok(permit)
    }
}

pub struct Actor {
    /// Whether a refusal is also told in a notification of the desktop (not in the tests, which have no desktop of their own).
    notify: bool,
    ctx: Arc<HostCtx>,
    gate: Arc<Gate>,
    runner: Arc<Runner>,
}

impl Actor {
    pub fn new(ctx: HostCtx) -> Actor {
        Actor { notify: true, ..Actor::with_runner(ctx, chooserhost::run) }
    }

    /// A request that was not served: a line on standard error and a notification, then the app is told "cancelled" (the router maps the code).
    fn refused(&self, writer: &Writer, token: u32, why: &str) {
        eprintln!("panpak: file dialog refused: {why}");
        if self.notify {
            chooserhost::notify("Panpak", &format!("The file dialog could not be shown: {why}."));
        }
        write_answer(writer, token, 2, None);
    }

    /// With another way to show the dialog (tests).
    pub fn with_runner(ctx: HostCtx, runner: impl Fn(&HostCtx, &Call) -> Outcome + Send + Sync + 'static) -> Actor {
        Actor { notify: false, ctx: Arc::new(ctx), gate: Arc::new(Gate { busy: AtomicBool::new(false), recent: Mutex::new(VecDeque::new()) }), runner: Arc::new(runner) }
    }

    /// A request frame of kind file chooser: the token of the router and the call of the app. Answered on `writer` when the dialog ends.
    pub fn request(&self, payload: &[u8], writer: Writer) {
        let Some((token, raw)) = filechooser::parse_request_payload(payload) else { return };
        let permit = match self.gate.try_enter(Instant::now()) {
            Ok(p) => p,
            Err(why) => {
                self.refused(&writer, token, why);
                return;
            }
        };
        let doc_prefix = format!("{}/doc/", self.ctx.runtime_dir);
        // The call is parsed again here: the router is not trusted to have filtered it.
        let call = match filechooser::parse_call(raw, &doc_prefix) {
            Ok(c) => c,
            Err(e) => {
                self.refused(&writer, token, &panpak_core::err_text(&e));
                return;
            }
        };
        let (ctx, runner) = (self.ctx.clone(), self.runner.clone());
        let spawned = std::thread::Builder::new().name("file-dialog".into()).spawn({
            let writer = writer.clone();
            move || {
                let outcome = runner(&ctx, &call);
                // The copy is free again before the answer is written: the router may send the dialog that waits as soon as it reads the answer.
                drop(permit);
                match outcome {
                    Outcome::Done(dict) => write_answer(&writer, token, 0, Some(&dict)),
                    Outcome::Ended { code, log, user } => {
                        if code != 1 {
                            eprintln!("panpak: file dialog: {log}");
                        }
                        if let Some(u) = user {
                            chooserhost::notify("Panpak", &u);
                        }
                        write_answer(&writer, token, code, None);
                    }
                }
            }
        });
        if spawned.is_err() {
            write_answer(&writer, token, 2, None);
        }
    }
}

/// Answer a request that cannot be served: ended, with a line on standard error.
pub fn answer_ended(writer: &Writer, payload: &[u8], why: &str) {
    if let Some((token, _)) = filechooser::parse_request_payload(payload) {
        eprintln!("panpak: file dialog refused: {why}");
        write_answer(writer, token, 2, None);
    }
}

fn write_answer(writer: &Writer, token: u32, code: u32, dict: Option<&Value>) {
    let text = filechooser::response_text(token, code, dict);
    let Ok(frame) = frames::encode_chooser_response(&text) else {
        // Too large to carry: the app is told that the dialog ended.
        if let Ok(f) = frames::encode_chooser_response(&filechooser::response_text(token, 2, None)) {
            if let Ok(mut w) = writer.lock() {
                let _ = w.write_all(&f);
            }
        }
        return;
    };
    if let Ok(mut w) = writer.lock() {
        let _ = w.write_all(&frame);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use panpak_core::dbus::{encode_message, Message, CALL};
    use std::io::Read;
    use std::sync::OnceLock;

    fn ctx() -> HostCtx {
        HostCtx { app_id: "panpak.org.example.App".into(), runtime_dir: "/run/user/1000".into(), writable_roots: Arc::new(OnceLock::new()) }
    }

    fn call_bytes(member: &str, opts: Vec<Value>) -> Vec<u8> {
        let m = Message {
            kind: CALL,
            serial: 9,
            path: Some("/org/freedesktop/portal/desktop".into()),
            interface: Some(filechooser::IFACE.into()),
            member: Some(member.into()),
            destination: Some("org.freedesktop.portal.Desktop".into()),
            signature: "ssa{sv}".into(),
            body: vec![Value::str("wayland:abc"), Value::str("Open a file"), Value::Array("{sv}".into(), opts)],
            ..Default::default()
        };
        encode_message(&m).unwrap()
    }

    fn entry(k: &str, v: Value) -> Value {
        Value::Entry(Box::new(Value::str(k)), Box::new(Value::variant(v)))
    }

    /// Run one request and read the frame that comes back.
    fn ask(actor: &Actor, token: u32, raw: &[u8]) -> (u32, u32, Option<Value>) {
        let (a, mut b) = UnixStream::pair().unwrap();
        b.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        actor.request(&filechooser::request_payload(token, raw), Arc::new(Mutex::new(a)));
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            if let Ok(Some((r, _))) = frames::parse_response(&buf) {
                assert_eq!(r.status, frames::Status::Chooser);
                return filechooser::parse_response_text(&r.text).expect("a valid answer");
            }
            let n = b.read(&mut chunk).unwrap();
            assert!(n > 0, "the actor closed without an answer");
            buf.extend_from_slice(&chunk[..n]);
        }
    }

    #[test]
    fn the_actor_filters_the_call_runs_the_dialog_and_answers_with_the_token() {
        let seen: Arc<Mutex<Vec<Call>>> = Arc::new(Mutex::new(Vec::new()));
        let s2 = seen.clone();
        let actor = Actor::with_runner(ctx(), move |_, call| {
            s2.lock().unwrap().push(call.clone());
            Outcome::Done(filechooser::result_dict(&["file:///run/user/1000/doc/ab/a.txt".to_string()], &[]))
        });
        let raw = call_bytes(
            "OpenFile",
            vec![
                entry("multiple", Value::Bool(true)),
                entry("accept_label", Value::str("Open")),
                entry("evil", Value::str("x")),
                entry("current_folder", Value::Array("y".into(), b"/home/user/secret\0".iter().map(|b| Value::Byte(*b)).collect())),
                entry("current_name", Value::str("ignored for OpenFile")),
            ],
        );
        let (token, code, dict) = ask(&actor, 7, &raw);
        assert_eq!((token, code), (7, 0));
        assert!(dict.is_some());
        let calls = seen.lock().unwrap();
        let keys: Vec<&str> = calls[0].opts.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["multiple", "accept_label"], "unknown keys, a host folder and a key for another method are dropped");
    }

    #[test]
    fn the_copy_is_free_before_the_answer_is_written() {
        // The router sends the dialog that waits as soon as it reads the answer of the one before: the gate must be open by then.
        let actor = Actor::with_runner(ctx(), |_, _| Outcome::Done(filechooser::result_dict(&["file:///run/user/1000/doc/ab/a.txt".to_string()], &[])));
        let good = call_bytes("OpenFile", vec![]);
        for i in 0..PER_MINUTE as u32 {
            assert_eq!(ask(&actor, i, &good).1, 0, "dialog {i}");
        }
    }

    #[test]
    fn a_bad_call_a_second_dialog_and_too_many_dialogs_are_answered_as_ended() {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let rx = Mutex::new(rx);
        let actor = Actor::with_runner(ctx(), move |_, _| {
            let _ = rx.lock().unwrap().recv_timeout(Duration::from_secs(5));
            Outcome::Ended { code: 1, log: "closed".into(), user: None }
        });
        // Not a D-Bus message.
        assert_eq!(ask(&actor, 1, b"nonsense").1, 2);
        // A parent window of another form.
        let m = Message {
            kind: CALL,
            serial: 1,
            path: Some("/p".into()),
            interface: Some(filechooser::IFACE.into()),
            member: Some("OpenFile".into()),
            signature: "ssa{sv}".into(),
            body: vec![Value::str("file:///etc"), Value::str("t"), Value::Array("{sv}".into(), vec![])],
            ..Default::default()
        };
        assert_eq!(ask(&actor, 2, &encode_message(&m).unwrap()).1, 2);
        // While one dialog is open, the next one is refused at once.
        let good = call_bytes("OpenFile", vec![]);
        let (a, mut b) = UnixStream::pair().unwrap();
        actor.request(&filechooser::request_payload(3, &good), Arc::new(Mutex::new(a)));
        assert_eq!(ask(&actor, 4, &good).1, 2);
        tx.send(()).unwrap();
        let mut buf = [0u8; 4096];
        b.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let n = b.read(&mut buf).unwrap();
        let (r, _) = frames::parse_response(&buf[..n]).unwrap().unwrap();
        assert_eq!(filechooser::parse_response_text(&r.text).unwrap().1, 1, "the user closed the first dialog");
    }

    #[test]
    fn at_most_five_dialogs_a_minute() {
        let gate = Arc::new(Gate { busy: AtomicBool::new(false), recent: Mutex::new(VecDeque::new()) });
        let t0 = Instant::now();
        for i in 0..PER_MINUTE {
            assert!(gate.try_enter(t0 + Duration::from_secs(i as u64)).is_ok(), "{i}");
        }
        assert!(gate.try_enter(t0 + Duration::from_secs(10)).is_err());
        // Failed attempts do not keep the copy busy, and a minute later it works again.
        assert!(gate.try_enter(t0 + Duration::from_secs(62)).is_ok());
    }
}
