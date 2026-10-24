use crate::core::build::BuildRequest;
use futures::lock::Mutex;
use std::panic::AssertUnwindSafe;

#[derive(Default)]
pub(super) struct Coordinator(AssertUnwindSafe<Mutex<()>>);

pub(super) trait BuildTarget {
    type Error;
    async fn pending(&self, job: &BuildRequest) -> Result<bool, Self::Error>;
    async fn wait_for_previous(&self) -> Result<(), Self::Error>;
    async fn build_if_pending(&self, job: &BuildRequest) -> Result<(), Self::Error>;
}

impl Coordinator {
    pub async fn run<T: BuildTarget>(&self, target: &T, job: &BuildRequest) -> Result<(), T::Error> {
        if !target.pending(job).await? {
            return Ok(());
        }
        let _guard = self.0.lock().await;
        target.wait_for_previous().await?;
        target.build_if_pending(job).await
    }
}
