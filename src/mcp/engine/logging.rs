use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, Weak};

pub fn logging_level_rank(level: &str) -> Option<u8> {
    match level {
        "debug" => Some(0),
        "info" => Some(1),
        "notice" => Some(2),
        "warning" => Some(3),
        "error" => Some(4),
        "critical" => Some(5),
        "alert" => Some(6),
        "emergency" => Some(7),
        _ => None,
    }
}

const DEFAULT_MIN_LOG_RANK: u8 = 1;

pub struct LogSubscription {
    emit: Box<dyn Fn(&str, &str) -> bool + Send + Sync>,
    min_rank: AtomicU8,
}

impl LogSubscription {
    pub fn with_emitter(
        emit: impl Fn(&str, &str) -> bool + Send + Sync + 'static,
    ) -> Arc<LogSubscription> {
        Arc::new(LogSubscription {
            emit: Box::new(emit),
            min_rank: AtomicU8::new(DEFAULT_MIN_LOG_RANK),
        })
    }

    pub fn set_min_rank(&self, rank: u8) {
        self.min_rank.store(rank, Ordering::SeqCst);
    }

    pub fn notify(&self, level: &str, message: &str) {
        let _ = self.emit(level, message);
    }

    fn emit(&self, level: &str, message: &str) -> bool {
        let rank = logging_level_rank(level).unwrap_or(DEFAULT_MIN_LOG_RANK);
        if rank >= self.min_rank.load(Ordering::SeqCst) {
            return (self.emit)(level, message);
        }
        true
    }
}

#[derive(Clone, Default)]
pub struct LogBroadcaster {
    subscribers: Arc<Mutex<Vec<Weak<LogSubscription>>>>,
}

impl LogBroadcaster {
    pub fn subscribe(&self, subscription: Arc<LogSubscription>) {
        self.subscribers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(Arc::downgrade(&subscription));
    }

    pub fn log(&self, level: &str, message: &str) {
        eprintln!("[CodeGraph MCP] {message}");
        self.subscribers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .retain(|subscription| {
                subscription
                    .upgrade()
                    .is_some_and(|subscription| subscription.emit(level, message))
            });
    }
}
