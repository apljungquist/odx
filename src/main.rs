use std::{
    env,
    env::VarError,
    fmt::Debug,
    process::{exit, Command, ExitStatus},
};

use sentry::{
    protocol::SpanStatus, ClientInitGuard, ClientOptions, Level, Transaction, TransactionContext,
    TransactionOrSpan, User,
};

fn dsn() -> Result<String, VarError> {
    let key = match cfg!(debug_assertions) {
        true => "ODX_SANDBOX_DSN",
        false => "ODX_DSN",
    };
    env::var(key)
}
struct Guard {
    _client_init_guard: ClientInitGuard,
    transaction: Option<Transaction>,
    cmd: String,
}

impl Guard {
    fn new(cmd: &impl Debug) -> anyhow::Result<Self> {
        let cmd = format!("{cmd:?}");

        let client_init_guard = sentry::init((
            dsn()?,
            ClientOptions {
                release: sentry::release_name!(),
                traces_sample_rate: 1.0,
                ..Default::default()
            },
        ));

        ctrlc::set_handler(move || {})?;

        let ctx = TransactionContext::new(&cmd, "ui.action");
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
            cmd,
        })
    }

    fn finish(mut self, status: ExitStatus) {
        let cmd = self.cmd.as_str();
        let transaction = self.transaction.take().unwrap();

        if status.success() {
            transaction.set_status(SpanStatus::Ok);
            sentry::capture_message(&format!("{cmd} ({status})"), Level::Info);
        } else {
            transaction.set_status(SpanStatus::UnknownError);
            sentry::capture_message(&format!("{cmd} ({status})"), Level::Warning);
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
    let mut cmd = Command::new(program);
    cmd.args(args);
    let status = match Guard::new(&cmd) {
        Ok(guard) => {
            let status = cmd.status().unwrap();
            guard.finish(cmd.status().unwrap());
            status
        }
        Err(e) => {
            eprintln!("{:?}", e);
            cmd.status().unwrap()
        }
    };
    if let Some(code) = status.code() {
        exit(code);
    }
}
