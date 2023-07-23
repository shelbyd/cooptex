#[cfg(all(test, not(feature = "loom-tests")))]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn second_thread_retries() {
        let a = CoopMutex::new(42);
        let b = CoopMutex::new(43);

        let s1 = LockScope::new(0);
        let s2 = LockScope::new(1);

        tokio::spawn(async move {
            let x1 = s1.lock(&a).await.unwrap();
            let x2 = s2.lock(&b).await.unwrap();

            tokio::spawn(async move {
                let _ = s1.lock(&b).await.unwrap();
            }).await.unwrap();

            assert!(s2.lock(&a).await.is_err());

            drop((x1, x2));
        }).await.unwrap();
    }

    #[tokio::test]
    async fn first_thread_blocks() {
        let mutex = CoopMutex::new(42);

        let s1 = LockScope::new(0);
        let s2 = LockScope::new(1);

        tokio::spawn(async move {
            let lock = s2.lock(&mutex).await.unwrap();

            tokio::spawn(async move {
                assert_eq!(*s1.lock(&mutex).await.unwrap().unwrap(), 42);
            }).await.unwrap();

            tokio::time::sleep(Duration::from_millis(100)).await;
            drop(lock);
        }).await.unwrap();
    }

    #[tokio::test]
    async fn second_waits_if_not_holding_other_locks() {
        let mutex = CoopMutex::new(42);

        let s1 = LockScope::new(0);
        let s2 = LockScope::new(1);

        tokio::spawn(async move {
            let lock = s1.lock(&mutex).await;
            tokio::time::sleep(Duration::from_millis(100)).await;
            drop(lock);
        }).await.unwrap();

        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(*s2.lock(&mutex).await.unwrap().unwrap(), 42);
    }
}