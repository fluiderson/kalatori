use crate::{
    server::{
        CurrencyInfo, OrderInfo, OrderStatus, PaymentStatus, ServerInfo, TokenKind,
        WithdrawalStatus,
    },
    AccountId, Balance,
};
use subxt::ext::sp_core::crypto::Ss58Codec;
use tokio::task;

pub const MODULE: &str = module_path!();

pub async fn callback(
    path: String,
    order: String,
    recipient: AccountId,
    debug: Option<bool>,
    remark: Option<String>,
    amount: Balance,
    rpc_url: String,
    paym_acc: AccountId,
) {
    let req = ureq::post(&path);

    task::spawn_blocking(move || {
        let d = req
            .send_json(OrderStatus {
                order,
                payment_status: PaymentStatus::Paid,
                message: String::new(),
                recipient: recipient.to_ss58check(),
                server_info: ServerInfo {
                    version: env!("CARGO_PKG_VERSION"),
                    instance_id: String::new(),
                    debug,
                    kalatori_remark: remark,
                },
                order_info: OrderInfo {
                    withdrawal_status: WithdrawalStatus::Waiting,
                    amount: amount.format(6),
                    currency: CurrencyInfo {
                        currency: "USDC".into(),
                        chain_name: "assethub-polkadot".into(),
                        kind: TokenKind::Asset,
                        decimals: 6,
                        rpc_url,
                        asset_id: Some(1337),
                    },
                    callback: path,
                    transactions: vec![],
                    payment_account: paym_acc.to_ss58check(),
                },
            })
            .unwrap();
    })
    .await
    .unwrap();
}
