//! A debug session shaped after the Debug Adapter Protocol.
//!
//! The VM runs on its own thread. The client sends [`Request`]s and
//! receives [`Message`]s (responses and events) over channels:
//!
//! | DAP                         | here                                   |
//! |-----------------------------|----------------------------------------|
//! | `launch` / `initialized`    | [`Session::launch`]                    |
//! | `setBreakpoints`            | [`Request::SetBreakpoints`]            |
//! | `setExceptionBreakpoints`   | [`Request::SetExceptionFilters`]       |
//! | `breakpointLocations`       | [`Request::BreakpointLocations`]       |
//! | `configurationDone`         | [`Request::ConfigurationDone`]         |
//! | `continue`/`next`/`stepIn`/`stepOut` | [`Request::Continue`] ...      |
//! | `pause`                     | [`Session::pause`] (asynchronous flag) |
//! | `stackTrace`/`scopes`/`variables` | [`Request::StackTrace`] ...      |
//! | `evaluate`                  | [`Request::Evaluate`]                  |
//! | `exceptionInfo`             | [`Request::ExceptionInfo`]             |
//! | `terminate`                 | [`Request::Terminate`]                 |
//! | `stopped`/`output`/`exited` events | [`Event`]                       |
//!
//! Before `ConfigurationDone` the program has not started; afterwards,
//! inspection requests are served only while the program is stopped.
//! A wire-level DAP server is a JSON codec over this API (one thread, one
//! VM, `threadId` 1).

use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use dj_bytecode as bc;
pub use dj_vm::{
    Breakpoint, BreakpointSpec, ExceptionFilters, ExceptionInfo, FrameInfo, ResumeAction, ScopeInfo, StopReason, Variable,
};
use dj_vm::{DebugHandler, DebugView, Vm, VmOptions};

#[derive(Clone, Debug)]
pub enum Request {
    SetBreakpoints(Vec<BreakpointSpec>),
    SetExceptionFilters(ExceptionFilters),
    /// Statement locations `(line, column)` on a line.
    BreakpointLocations(u32),
    ConfigurationDone,
    Continue,
    Next,
    StepIn,
    StepOut,
    StackTrace,
    Scopes { frame: usize },
    Variables { reference: u32 },
    Evaluate { frame: usize, expression: String },
    ExceptionInfo,
    Terminate,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Response {
    Ack,
    Breakpoints(Vec<Breakpoint>),
    Locations(Vec<(u32, u32)>),
    StackTrace(Vec<FrameInfo>),
    Scopes(Vec<ScopeInfo>),
    Variables(Vec<Variable>),
    Evaluate(Result<Variable, String>),
    ExceptionInfo(Option<ExceptionInfo>),
    Error(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Stopped { reason: StopReason, frame: Option<FrameInfo> },
    Output(String),
    /// The program finished; `error` holds an uncaught exception (or the
    /// termination notice).
    Exited { error: Option<String> },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    Response(Response),
    Event(Event),
}

#[derive(Clone, Debug, Default)]
pub struct LaunchConfig {
    pub stop_on_entry: bool,
    pub gc_stress: bool,
}

pub struct Session {
    tx: Sender<Request>,
    rx: Receiver<Message>,
    pause: Arc<AtomicBool>,
    pending: std::cell::RefCell<std::collections::VecDeque<Event>>,
    thread: Option<JoinHandle<()>>,
}

const TIMEOUT: Duration = Duration::from_secs(20);

impl Session {
    /// Start a VM thread with `module` loaded; the program starts after
    /// [`Request::ConfigurationDone`].
    pub fn launch(module: bc::Module, config: LaunchConfig) -> Session {
        let (tx, req_rx) = channel::<Request>();
        let (out_tx, rx) = channel::<Message>();
        let (pause_tx, pause_rx) = channel::<Arc<AtomicBool>>();
        let thread = std::thread::Builder::new()
            .name("djungle-debuggee".into())
            .stack_size(256 << 20)
            .spawn(move || vm_thread(module, config, req_rx, out_tx, pause_tx))
            .expect("spawn debuggee thread");
        let pause = pause_rx.recv().unwrap_or_default();
        Session { tx, rx, pause, pending: Default::default(), thread: Some(thread) }
    }

    /// Send a request and wait for its response (events received meanwhile
    /// are queued for [`Session::next_event`]).
    pub fn request(&self, r: Request) -> Response {
        if self.tx.send(r).is_err() {
            return Response::Error("debuggee is gone".into());
        }
        loop {
            match self.rx.recv_timeout(TIMEOUT) {
                Ok(Message::Response(r)) => return r,
                Ok(Message::Event(e)) => self.pending.borrow_mut().push_back(e),
                Err(RecvTimeoutError::Timeout) => return Response::Error("timeout (is the program running?)".into()),
                Err(RecvTimeoutError::Disconnected) => return Response::Error("debuggee is gone".into()),
            }
        }
    }

    pub fn next_event(&self, timeout: Duration) -> Option<Event> {
        if let Some(e) = self.pending.borrow_mut().pop_front() {
            return Some(e);
        }
        loop {
            match self.rx.recv_timeout(timeout) {
                Ok(Message::Event(e)) => return Some(e),
                Ok(Message::Response(_)) => continue,
                Err(_) => return None,
            }
        }
    }

    /// Next event that is not output (output is appended to `output`).
    pub fn next_stop_or_exit(&self, output: &mut String) -> Option<Event> {
        loop {
            match self.next_event(TIMEOUT)? {
                Event::Output(s) => output.push_str(&s),
                e => return Some(e),
            }
        }
    }

    /// Request an asynchronous pause (takes effect at the next instruction).
    pub fn pause(&self) {
        self.pause.store(true, Ordering::Relaxed);
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.tx.send(Request::Terminate);
        if let Some(t) = self.thread.take() {
            let (tx, _) = channel::<Request>();
            // Close our request channel so a blocked debuggee wakes up.
            drop(std::mem::replace(&mut self.tx, tx));
            let _ = t.join();
        }
    }
}

struct Handler {
    module: u32,
    requests: Rc<Receiver<Request>>,
    out: Sender<Message>,
}

impl Handler {
    fn respond(&self, r: Response) {
        let _ = self.out.send(Message::Response(r));
    }
}

fn locations(vm: &Vm, module: u32, line: u32) -> Vec<(u32, u32)> {
    vm.module(module)
        .and_then(|m| m.bytecode.debug.as_ref())
        .map(|d| d.breakpoint_locations(line).into_iter().map(|b| (b.pos.line, b.pos.col)).collect())
        .unwrap_or_default()
}

impl DebugHandler for Handler {
    fn on_stop(&mut self, view: &mut DebugView<'_>, reason: StopReason) -> ResumeAction {
        let frame = view.frames().into_iter().next();
        let _ = self.out.send(Message::Event(Event::Stopped { reason, frame }));
        loop {
            let Ok(req) = self.requests.recv() else { return ResumeAction::Terminate };
            let action = match req {
                Request::Continue => Some(ResumeAction::Continue),
                Request::Next => Some(ResumeAction::StepOver),
                Request::StepIn => Some(ResumeAction::StepIn),
                Request::StepOut => Some(ResumeAction::StepOut),
                Request::Terminate => Some(ResumeAction::Terminate),
                _ => None,
            };
            if let Some(a) = action {
                self.respond(Response::Ack);
                return a;
            }
            let r = match req {
                Request::StackTrace => Response::StackTrace(view.frames()),
                Request::Scopes { frame } => Response::Scopes(view.scopes(frame)),
                Request::Variables { reference } => Response::Variables(view.variables(reference)),
                Request::Evaluate { frame, expression } => Response::Evaluate(view.evaluate(frame, &expression)),
                Request::ExceptionInfo => Response::ExceptionInfo(view.exception()),
                Request::SetBreakpoints(specs) => Response::Breakpoints(view.set_breakpoints(self.module, &specs)),
                Request::SetExceptionFilters(f) => {
                    view.set_exception_filters(f);
                    Response::Ack
                }
                Request::BreakpointLocations(line) => Response::Locations(locations(view.vm(), self.module, line)),
                Request::ConfigurationDone => Response::Error("already configured".into()),
                _ => unreachable!(),
            };
            self.respond(r);
        }
    }

    fn on_output(&mut self, text: &str) {
        let _ = self.out.send(Message::Event(Event::Output(text.to_string())));
    }
}

fn vm_thread(module: bc::Module, config: LaunchConfig, req_rx: Receiver<Request>, out: Sender<Message>, pause_tx: Sender<Arc<AtomicBool>>) {
    let mut vm = Vm::new(VmOptions { capture_output: true, gc_stress: config.gc_stress, ..VmOptions::default() });
    let _ = pause_tx.send(vm.pause_handle());
    let send = |m: Message| {
        let _ = out.send(m);
    };
    let id = match vm.load(module) {
        Ok(id) => id,
        Err(e) => {
            send(Message::Event(Event::Exited { error: Some(e.to_string()) }));
            return;
        }
    };
    let requests = Rc::new(req_rx);
    // Configuration phase.
    loop {
        let Ok(req) = requests.recv() else { return };
        let r = match req {
            Request::SetBreakpoints(specs) => Response::Breakpoints(vm.set_breakpoints(id, &specs)),
            Request::SetExceptionFilters(f) => {
                vm.set_exception_filters(f);
                Response::Ack
            }
            Request::BreakpointLocations(line) => Response::Locations(locations(&vm, id, line)),
            Request::ConfigurationDone => {
                send(Message::Response(Response::Ack));
                break;
            }
            Request::Terminate => {
                send(Message::Response(Response::Ack));
                send(Message::Event(Event::Exited { error: Some("terminated before start".into()) }));
                return;
            }
            _ => Response::Error("the program has not started".into()),
        };
        send(Message::Response(r));
    }
    vm.set_stop_on_entry(config.stop_on_entry);
    vm.set_debug_handler(Box::new(Handler { module: id, requests: requests.clone(), out: out.clone() }));
    let result = vm.run_module(id);
    vm.clear_debug_handler();
    send(Message::Event(Event::Exited { error: result.err().map(|u| u.to_string()) }));
    // Answer stragglers until the client hangs up.
    while let Ok(req) = requests.recv() {
        if matches!(req, Request::Terminate) {
            send(Message::Response(Response::Ack));
            return;
        }
        send(Message::Response(Response::Error("the program has exited".into())));
    }
}
