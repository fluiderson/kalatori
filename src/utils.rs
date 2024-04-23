use crate::error::{ErrorUtil, NotHex};

pub fn unhex(hex_data: &str, what_is_hex: NotHex) -> Result<Vec<u8>, ErrorUtil> {
    if let Some(stripped) = hex_data.strip_prefix("0x") {
        hex::decode(stripped).map_err(|_| ErrorUtil::NotHex(what_is_hex))
    } else {
        hex::decode(hex_data).map_err(|_| ErrorUtil::NotHex(what_is_hex))
    }
}
