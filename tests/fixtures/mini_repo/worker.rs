use crate::payments::save_payment;

pub struct RetryWorker;

impl RetryWorker {
    pub async fn retry_charge(&self, payment_id: &str) -> bool {
        save_payment(payment_id)
    }
}
