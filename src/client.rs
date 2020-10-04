use crate::broker::{BrokerMsg, Connected, JoinErr, JoinResult};
use async_tungstenite::tungstenite::{error::Error as WsErr, Message as WsMsg};
use futures_util::{
    sink::{Sink, SinkExt},
    stream::{self, Stream, StreamExt},
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json;
use smol::{
    channel::{self, Receiver, Sender},
    pin,
};
use spyfall::{PlayerId, RoomId};
use std::pin::Pin;

/// What the client actor receives from the browser
#[derive(Debug, Clone)]
pub enum ClientMsg {
    Join(Join, Sender<JoinResult>),
    Room(RoomMsg),
}

#[derive(Debug, Clone, Deserialize)]
pub struct Join {
    pub room: Option<RoomId>,
    pub name: PlayerId,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub enum RoomMsg {
    Leave { room: RoomId, name: PlayerId },
    Start { room: RoomId },
}

/// what the client actor sends back to the browser
/// if assignment is `None`, the player is the spy

#[derive(Debug)]
pub enum ParseErr {
    MalformedMsg(serde_json::Error),
    NonTextMsg,
    ClientDisconnected,
}

impl std::fmt::Display for ParseErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MalformedMsg(serde_err) => write!(
                f,
                "message sent could not be parsed as a valid type, more details below:\n{}",
                serde_err
            ),
            Self::NonTextMsg => write!(f, "message was not of supported type"),
            Self::ClientDisconnected => write!(
                f,
                "the client disconnected early while expecting a message from the client"
            ),
        }
    }
}

impl std::error::Error for ParseErr {}

// general control flow ADT
enum Either<A, B> {
    Left(A),
    Right(B),
}

pub async fn client_actor(
    websocket: impl Stream<Item = Result<WsMsg, WsErr>> + Sink<WsMsg, Error = WsErr> + Unpin + Send,
    broker_tx: Sender<ClientMsg>,
) -> Result<(), Box<dyn std::error::Error + Sync + Send>> {
    let (ws_sink, ws_stream) = websocket.split();
    // pin these to the stack and make them mutable
    pin!(ws_sink, ws_stream);

    let first_msg = ws_stream
        .next()
        .await
        .ok_or_else(|| err_msg("A general Websocket Error"))??;
    let join_msg = parse_msg::<Join>(first_msg)?;
    let name = join_msg.name.clone();
    let (join_tx, join_rx) = channel::bounded(1);
    broker_tx
        .send(ClientMsg::Join(join_msg.clone(), join_tx))
        .await?;
    let (room_rx_opt, join_res) = transpose_join_res(join_rx.recv().await?);
    send_back_msg(&join_res, &mut ws_sink).await?;

    if let Some((room_rx, room)) = room_rx_opt {
        let dropped = client_room_state(
            room_rx,
            &broker_tx,
            &mut ws_stream,
            &mut ws_sink,
            &join_msg.name,
        )
        .await;
        if let Err(_) = dropped {
            broker_tx
                .send(ClientMsg::Room(RoomMsg::Leave { room, name }))
                .await?;
        }
        dropped?;
    }

    Ok(())
}

/// Use a separate function when the client has joined a room. This is so if the client encounters any errors,
/// the parent function can notify the broker the client has dropped
/// NOTE: would have been nicer to keep this in the parent function (indicated by the arity of this fn),
//        look into changing this to a try block when that is stablilized.
async fn client_room_state<R, W>(
    room_rx: Receiver<BrokerMsg>,
    broker_tx: &Sender<ClientMsg>,
    ws_stream: &mut Pin<&mut R>,
    ws_sink: &mut Pin<&mut W>,
    player: &PlayerId,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    R: Stream<Item = Result<WsMsg, WsErr>>,
    W: Sink<WsMsg, Error = WsErr>,
{
    let room_rx = room_rx.map(|msg| Either::Left(msg));
    let ws_stream = ws_stream.map(|msg| Either::Right(msg));
    let mut select_stream = stream::select(ws_stream, room_rx);
    while let Some(either) = select_stream.next().await {
        match either {
            Either::Left(broker_msg) => {
                println!(
                    "(Player {}) Dealing with broker message {:?}",
                    player, broker_msg
                );
                // seems to be an issue with sendin back these messages
                send_back_msg(&broker_msg, ws_sink).await?;
            }
            Either::Right(ws_msg_res) => {
                let ws_msg = ws_msg_res?;
                println!(
                    "(Player {}) Dealing with room message from the websocket {}",
                    player, ws_msg
                );
                let room_msg = parse_msg::<RoomMsg>(ws_msg)?;
                let exit = matches!(room_msg, RoomMsg::Leave { .. });
                broker_tx.send(ClientMsg::Room(room_msg)).await?;
                if exit {
                    break;
                }
            }
        };
    }

    Ok(())
}

/// Send a serialize-able message back to the websocket
async fn send_back_msg<W, S>(
    msg: &S,
    ws_write: &mut Pin<&mut W>,
) -> Result<(), Box<dyn std::error::Error + Sync + Send>>
where
    W: Sink<WsMsg, Error = WsErr>,
    S: Serialize,
{
    let serialized_msg = serde_json::to_string(msg).expect("Failed to serialize msg back");
    ws_write.send(WsMsg::text(serialized_msg)).await?;
    Ok(())
}

/// retreieve a deserialize-able message from the websocket
/// (and deal with the many errors the websocket can present)
pub fn parse_msg<D: DeserializeOwned>(ws_msg: WsMsg) -> Result<D, ParseErr> {
    match ws_msg {
        WsMsg::Text(txt) => serde_json::from_str::<D>(&txt).map_err(|e| ParseErr::MalformedMsg(e)),
        WsMsg::Close(_) => Err(ParseErr::ClientDisconnected),
        _ => Err(ParseErr::NonTextMsg),
    }
}

/// Convert the broker's join result into a tuple of the info the client_actor will need
//// and a serialize-able message that can be sent back to the client
fn transpose_join_res(
    join_res: JoinResult,
) -> (
    Option<(Receiver<BrokerMsg>, String)>,
    Result<Connected, JoinErr>,
) {
    match join_res {
        Ok((conn, rx)) => (Some((rx, conn.room_id.clone())), Ok(conn)),
        Err(err) => (None, Err(err)),
    }
}

/// make static string error descriptions easier to deal with
fn err_msg(err_description: &'static str) -> Box<dyn std::error::Error + Sync + Send> {
    Box::from(String::from(err_description))
}
