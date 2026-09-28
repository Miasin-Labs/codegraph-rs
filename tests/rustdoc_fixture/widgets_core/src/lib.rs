pub mod task {
    pub enum Poll<T> {
        Ready(T),
        Pending,
    }

    impl<T> Poll<T> {
        pub fn is_ready(&self) -> bool {
            matches!(self, Poll::Ready(_))
        }
    }
}

pub trait Named {
    fn name(&self) -> String;

    fn greeting(&self) -> String {
        format!("hello {}", self.name())
    }
}

pub trait Shout {
    fn shout(&self) -> String;
}

impl<T: Named + ?Sized> Shout for T {
    fn shout(&self) -> String {
        self.name().to_uppercase()
    }
}
