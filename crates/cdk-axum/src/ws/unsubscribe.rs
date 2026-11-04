use cdk::ws::{WsResponseResult, WsUnsubscribeRequest};
use cdk_common::database::MintStore;

use super::{WsContext, WsError};

pub(crate) async fn handle<D>(
    context: &mut WsContext<D>,
    req: WsUnsubscribeRequest,
) -> Result<WsResponseResult, WsError>
where
    D: MintStore,
{
    if let Some(handle) = context.subscriptions.remove(&req.sub_id) {
        handle.abort();
        Ok(WsResponseResult {
            status: "OK".to_string(),
            sub_id: req.sub_id,
        })
    } else {
        Err(WsError::InvalidParams)
    }
}
