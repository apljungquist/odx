use std::{
    env,
    env::VarError,
    process::{exit, Command, ExitStatus},
    str::FromStr,
};

use sentry::{
    protocol::{SpanStatus, TraceId},
    ClientInitGuard, ClientOptions, Level, Transaction, TransactionContext, TransactionOrSpan,
    User,
};

fn dsn() -> Result<String, VarError> {
    let key = match cfg!(debug_assertions) {
        true => "ODX_SANDBOX_DSN",
        false => "ODX_DSN",
    };
    env::var(key)
}

const TRACE_ID_KEY: &str = "ODX_TRACE_ID";
fn trace_id() -> Option<TraceId> {
    let s = env::var(TRACE_ID_KEY).ok()?;
    TraceId::from_str(&s).ok()
}

fn basename(path: &str) -> &str {
    path.rsplit('/')
        .next()
        .expect("split returns at least one element")
}

struct Guard {
    _client_init_guard: ClientInitGuard,
    transaction: Option<Transaction>,
    trace_id: TraceId,
    cmd: String,
}

impl Guard {
    fn new(program: &str, args: &[String]) -> anyhow::Result<Self> {
        let program = basename(program);
        let cmd = format!("{program} {}", args.join(" "));

        let client_init_guard = sentry::init((
            dsn()?,
            ClientOptions {
                release: sentry::release_name!(),
                traces_sample_rate: 1.0,
                ..Default::default()
            },
        ));

        ctrlc::set_handler(move || {})?;

        let trace_id = trace_id().unwrap_or_default();
        let ctx = TransactionContext::new_with_trace_id(&cmd, "ui.action", trace_id);
        let transaction = sentry::start_transaction(ctx);

        sentry::configure_scope(|scope| {
            scope.set_span(Some(TransactionOrSpan::Transaction(transaction.clone())));

            if let Ok(username) = env::var("USER") {
                scope.set_user(Some(User {
                    username: Some(username),
                    ..Default::default()
                }));
            }
        });

        Ok(Self {
            _client_init_guard: client_init_guard,
            transaction: Some(transaction),
            trace_id,
            cmd,
        })
    }

    fn finish(mut self, status: ExitStatus) {
        let cmd = self.cmd.as_str();
        let transaction = self.transaction.take().unwrap();

        if status.success() {
            transaction.set_status(SpanStatus::Ok);
            sentry::capture_message(&format!("{cmd} succeeded ({status})"), Level::Info);
        } else {
            transaction.set_status(SpanStatus::UnknownError);
            sentry::capture_message(&format!("{cmd} failed ({status})"), Level::Warning);
        }
        transaction.finish();
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(transaction) = self.transaction.take() {
            transaction.set_status(SpanStatus::UnknownError);
            sentry::capture_message(&format!("{} did not exit", &self.cmd), Level::Error);
            transaction.finish();
        }
    }
}

fn main() {
    let mut args = env::args().skip(1);
    let program = args.next().unwrap();
    let args: Vec<String> = args.collect();

    let status = match Guard::new(&program, &args) {
        Ok(guard) => {
            let status = Command::new(program)
                .args(args)
                .env(TRACE_ID_KEY, guard.trace_id.to_string())
                .status()
                .unwrap();
            guard.finish(status);
            status
        }
        Err(e) => {
            eprintln!("{:?}", e);
            Command::new(program).args(args).status().unwrap()
        }
    };
    if let Some(code) = status.code() {
        exit(code);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn basename_works_on_example() {
        assert_eq!(basename("/home/user/example"), "example");
    }
}
