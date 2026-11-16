use std::{collections::HashMap, time::Duration};

use tokio::{sync::Mutex, time::Instant};
use uuid::Uuid;

use super::{
    protocol::{API_VERSION, NotificationReceipt, NotificationStatus, NotificationStatusResponse},
    service::NotificationError,
};

const RECEIPT_TIMEOUT: Duration = Duration::from_secs(30);
const RETENTION: Duration = Duration::from_secs(15 * 60);
const MAX_RECORDS: usize = 1024;

#[derive(Debug)]
struct Record {
    created_at: Instant,
    receipt: Option<NotificationReceipt>,
}

#[derive(Debug, Default)]
pub(super) struct NotificationTracker {
    records: Mutex<HashMap<String, Record>>,
}

impl NotificationTracker {
    pub async fn register(&self) -> Result<String, NotificationError> {
        let mut records = self.records.lock().await;
        Self::prune(&mut records);
        if records.len() >= MAX_RECORDS {
            return Err(NotificationError::Capacity);
        }
        let id = Uuid::new_v4().to_string();
        records.insert(
            id.clone(),
            Record {
                created_at: Instant::now(),
                receipt: None,
            },
        );
        Ok(id)
    }

    pub async fn remove(&self, id: &str) {
        self.records.lock().await.remove(id);
    }

    pub async fn get(&self, id: &str) -> Result<NotificationStatusResponse, NotificationError> {
        let mut records = self.records.lock().await;
        Self::prune(&mut records);
        let record = records.get(id).ok_or(NotificationError::NotFound)?;
        Ok(Self::response(id, record))
    }

    pub async fn receive(
        &self,
        id: &str,
        receipt: NotificationReceipt,
    ) -> Result<NotificationStatusResponse, NotificationError> {
        if let NotificationReceipt::Failed { code, message } = &receipt
            && (code.trim().is_empty() || message.trim().is_empty())
        {
            return Err(NotificationError::InvalidRequest(
                "失败回执的 code 和 message 不能为空".to_owned(),
            ));
        }
        let mut records = self.records.lock().await;
        Self::prune(&mut records);
        let record = records.get_mut(id).ok_or(NotificationError::NotFound)?;
        match &record.receipt {
            Some(existing) if existing != &receipt => {
                return Err(NotificationError::ReceiptConflict);
            }
            Some(_) => {}
            None => record.receipt = Some(receipt),
        }
        Ok(Self::response(id, record))
    }

    fn prune(records: &mut HashMap<String, Record>) {
        // 在访问时清理，避免为通知状态启动独立线程；容量上限约束空闲期间的占用。
        records.retain(|_, record| record.created_at.elapsed() < RETENTION);
    }

    fn response(id: &str, record: &Record) -> NotificationStatusResponse {
        let (status, code, message) = match &record.receipt {
            Some(NotificationReceipt::Submitted {}) => (NotificationStatus::Submitted, None, None),
            Some(NotificationReceipt::Failed { code, message }) => (
                NotificationStatus::Failed,
                Some(code.clone()),
                Some(message.clone()),
            ),
            None if record.created_at.elapsed() >= RECEIPT_TIMEOUT => {
                (NotificationStatus::Unconfirmed, None, None)
            }
            None => (NotificationStatus::Forwarded, None, None),
        };
        NotificationStatusResponse {
            api_version: API_VERSION.to_owned(),
            notification_id: id.to_owned(),
            status,
            code,
            message,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn accepts_late_receipt_until_expiry_without_extending_retention() {
        let tracker = NotificationTracker::default();
        let id = tracker.register().await.unwrap();
        assert_eq!(
            tracker.get(&id).await.unwrap().status,
            NotificationStatus::Forwarded
        );
        tokio::time::advance(RECEIPT_TIMEOUT).await;
        assert_eq!(
            tracker.get(&id).await.unwrap().status,
            NotificationStatus::Unconfirmed
        );
        assert_eq!(
            tracker
                .receive(&id, NotificationReceipt::Submitted {})
                .await
                .unwrap()
                .status,
            NotificationStatus::Submitted
        );
        tokio::time::advance(RETENTION - RECEIPT_TIMEOUT).await;
        assert!(matches!(
            tracker.get(&id).await,
            Err(NotificationError::NotFound)
        ));
        assert!(matches!(
            tracker
                .receive(&id, NotificationReceipt::Submitted {})
                .await,
            Err(NotificationError::NotFound)
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn bounds_memory_and_releases_expired_capacity() {
        let tracker = NotificationTracker::default();
        for _ in 0..MAX_RECORDS {
            tracker.register().await.unwrap();
        }
        assert!(matches!(
            tracker.register().await,
            Err(NotificationError::Capacity)
        ));
        tokio::time::advance(RETENTION).await;
        tracker.register().await.unwrap();
        assert_eq!(tracker.records.lock().await.len(), 1);
    }
}
