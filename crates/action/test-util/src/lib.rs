mod test_context;

use std::future::Future;

pub use self::test_context::TestContext;

pub fn run<F>(future: F) -> F::Output
where
    F: Future,
{
    cealn_test_util::prep();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(future)
}
