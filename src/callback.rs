use crate::definitions::api_v2::OrderStatus;
use tokio::task;

pub const MODULE: &str = module_path!();

pub async fn callback(path: &str, order_status: OrderStatus) {
    let req = ureq::post(path);

    task::spawn_blocking(move || {
        let _d = req.send_json(order_status);
    })
    .await
    .unwrap();
}
