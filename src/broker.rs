use crate::client::{ClientMsg, Join, RoomMsg};
use base32;
use fastrand::Rng;
use serde::Serialize;
use smol::channel::{self, Receiver, Sender};
use spyfall::{find_index, AsyncErr, AsyncResult, PlayerId};
use std::collections::hash_map::{Entry, HashMap, OccupiedEntry, VacantEntry};
use std::sync::Arc;

const ROOM_ID_BYTES: usize = 5;
const MAX_ROOM_CREATION_ATTEMPTS: usize = 5;
const MIN_PLAYERS_TO_START_GAME: usize = 3;

type RoomId = String;
pub type JoinResult = Result<(Connected, Receiver<BrokerMsg>), JoinErr>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum BrokerMsg {
    Join(Arc<str>),
    Left(Arc<str>),
    Started(Start),
    NotEnoughPlayers,
}

// returned when successfully joining the room
#[derive(Debug, Clone, Serialize)]
pub struct Connected {
    pub room_id: String,
    pub players: Vec<String>,
}

// A user error when attempting to connect to the room
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum JoinErr {
    NoSuchRoom,
    UsernameTaken,
    FailedToCreateRoom,
}

// sent directly to client actors.
// Information is then decomposed by each actor to send the appropriate message back to the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameInfo {
    pub player_roles: HashMap<PlayerId, String>,
    pub location: String,
    pub first: PlayerId,
    pub spy: PlayerId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Start {
    assignment: Option<Assignment>,
    first: Arc<str>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Assignment {
    location: Arc<str>,
    role: String,
}

#[derive(Debug)]
pub struct Room {
    names: Vec<String>,
    senders: Vec<Sender<BrokerMsg>>,
}

impl PartialEq<Vec<String>> for Room {
    fn eq(&self, other: &Vec<String>) -> bool {
        self.names.eq(other)
    }
}

impl PartialEq<Room> for Room {
    fn eq(&self, other: &Self) -> bool {
        self.names.eq(&other.names)
    }
}

impl Eq for Room {}

// TODO: Use this instead of just the string name provied by the user.
// should consist of some kind of globally unique identifier + the user-submitted name
// struct Player {}

#[derive(Debug, PartialEq)]
pub struct RoomTable(HashMap<RoomId, Room>);

impl RoomTable {
    pub fn new() -> Self {
        Self(HashMap::new())
    }

    pub fn get_room_entry<'a>(
        &'a mut self,
        room_id: RoomId,
    ) -> Result<OccupiedEntry<'a, RoomId, Room>, JoinErr> {
        match self.0.entry(room_id) {
            Entry::Occupied(occupied) => Ok(occupied),
            _ => Err(JoinErr::NoSuchRoom),
        }
    }

    pub fn get_room(&self, room_id: &RoomId) -> Option<&Room> {
        self.0.get(room_id)
    }

    pub fn try_create_room<'a>(&'a mut self, rng: &Rng) -> Option<VacantEntry<'a, String, Room>> {
        let mut unique_room_id = None;
        // hacky way of getting around using mutable references in a loop
        for _ in 0..MAX_ROOM_CREATION_ATTEMPTS {
            let room_id = create_room_id(rng);
            if self.0.get(&room_id).is_none() {
                unique_room_id = Some(room_id);
                break;
            }
        }

        if let Some(room_id) = unique_room_id {
            if let Entry::Vacant(vacant) = self.0.entry(room_id) {
                return Some(vacant);
            }
        }

        None
    }

    // Attempts to remove a player from a room. Returns a mutable reference to the room if successful and the room still exists
    // (room may be evicted if it is empty)
    pub fn try_remove_player<'a>(&'a mut self, name: &PlayerId, room: RoomId) -> Option<&mut Room> {
        if let Entry::Occupied(mut room_entry) = self.0.entry(room) {
            let player_index = find_index(&room_entry.get().names, name);
            if let Some(index) = player_index {
                let room = room_entry.get_mut();
                room.names.remove(index);
                room.senders.remove(index);
            }

            if room_entry.get().names.is_empty() {
                room_entry.remove_entry();
            } else if player_index.is_some() {
                return Some(room_entry.into_mut());
            }
        }

        None
    }
}

impl From<RoomTable> for HashMap<RoomId, Room> {
    fn from(roomtable: RoomTable) -> HashMap<RoomId, Room> {
        roomtable.0
    }
}

fn create_room_id(rng: &Rng) -> String {
    let bytes = [rng.u8(..); ROOM_ID_BYTES];
    base32::encode(base32::Alphabet::Crockford, &bytes)
}

#[derive(Clone)]
struct SpyfallRepo {
    // mapping of locations and their associated roles
    roles: HashMap<String, Vec<String>>,
    locations: Vec<String>,
}

impl SpyfallRepo {
    fn new() -> Self {
        let roles_json = include_str!("../roles.json");
        let roles = serde_json::from_str::<HashMap<_, _>>(roles_json)
            .expect("Failed to parse the roles dataset, check role.json");
        let locations = roles.keys().cloned().collect();
        Self { roles, locations }
    }

    fn locations(&self) -> &[String] {
        &self.locations
    }

    fn roles(&self, location: &str) -> &[String] {
        &self.roles[location]
    }
}

pub async fn broker_actor(client_listener: Receiver<ClientMsg>) -> AsyncResult<RoomTable> {
    let rng = Rng::new();
    let mut rooms = RoomTable::new();
    let repo = SpyfallRepo::new();
    while let Ok(msg) = client_listener.recv().await {
        match msg {
            ClientMsg::Join(Join { room, name }, sender) => match room {
                Some(room_id) => {
                    println!("Adding player {} to room {}", name, room_id);
                    let join_res = add_player(&mut rooms, room_id, name).await?;
                    sender.send(join_res).await?;
                }
                // Create a new room
                _ => {
                    println!("Creating a new room for player: {}", name);
                    let msg_back = rooms
                        .try_create_room(&rng)
                        .map(|vacant_room| {
                            let room_id = vacant_room.key().clone();
                            let (sender, rx) = channel::bounded(1);
                            let players = vec![name];
                            let senders = vec![sender];
                            vacant_room.insert(Room {
                                names: players.clone(),
                                senders,
                            });
                            (Connected { room_id, players }, rx)
                        })
                        .ok_or(JoinErr::FailedToCreateRoom);
                    sender.send(msg_back).await?;
                }
            },
            ClientMsg::Room(room_msg) => match room_msg {
                // TODO: it would probably better to guarantee that the leave message is sent by the player who is leaving.
                // maybe create an associated UUID that ensures the correct client is sending these messages. Look at `Player` struct
                RoomMsg::Leave { name, room } => {
                    println!("Removing {} from room {}", name, room);
                    if let Some(room) = rooms.try_remove_player(&name, room) {
                        send_room(&room.senders, BrokerMsg::Left(Arc::from(name))).await?;
                    }
                }
                RoomMsg::Start { room } => {
                    if let Some(room) = rooms.get_room(&room) {
                        if room.names.len() < MIN_PLAYERS_TO_START_GAME {
                            send_room(&room.senders, BrokerMsg::NotEnoughPlayers).await?;
                        } else {
                            let names = room.names.clone();
                            let mut game_info = assign_roles(names, &repo, &rng);
                            let location = Arc::from(game_info.location);
                            let first = Arc::from(game_info.first);
                            for (name, sender) in room.names.iter().zip(&room.senders) {
                                let assignment = if *name == game_info.spy {
                                    None
                                } else {
                                    let role = game_info
                                        .player_roles
                                        .remove(name)
                                        .ok_or_else(|| format!("no role assigned to {}", name))?;
                                    Some(Assignment {
                                        role,
                                        location: Arc::clone(&location),
                                    })
                                };
                                sender
                                    .send(BrokerMsg::Started(Start {
                                        assignment,
                                        first: Arc::clone(&first),
                                    }))
                                    .await?;
                            }
                        };
                    }
                }
            },
        }
    }

    Ok(rooms)
}

// attemps to add a player
// the outermost error is a programatic error (unexpected)
// the inner result is what to send back to the client (errors of usage, and are expected)
async fn add_player(
    rooms: &mut RoomTable,
    room_id: RoomId,
    name: PlayerId,
) -> Result<JoinResult, AsyncErr> {
    let mut room_entry = match rooms.get_room_entry(room_id.clone()) {
        Ok(room_entry) => room_entry,
        Err(e) => return Ok(Err(e)),
    };

    if find_index(&room_entry.get().names, &name).is_none() {
        // message other players a new player is joining
        send_room(
            &room_entry.get().senders,
            BrokerMsg::Join(Arc::from(name.clone())),
        )
        .await?;

        let (sender, rx) = channel::bounded(1);
        // insert new player
        let room = room_entry.get_mut();
        room.names.push(name);
        room.senders.push(sender);
        let players = room.names.clone();

        Ok(Ok((Connected { players, room_id }, rx)))
    } else {
        Ok(Err(JoinErr::UsernameTaken))
    }
}

async fn send_room(senders: &[Sender<BrokerMsg>], msg: BrokerMsg) -> AsyncResult<()> {
    // split to avoid extra clone call
    if let Some((first, rest)) = senders.split_first() {
        for sender in rest {
            let clone = msg.clone();
            sender.send(clone).await?;
        }
        first.send(msg).await?;
    }
    Ok(())
}

fn assign_roles(mut players: Vec<String>, repo: &SpyfallRepo, rng: &Rng) -> GameInfo {
    let locations = repo.locations();
    let (first_player_index, spy_index) = (rng.usize(..players.len()), rng.usize(..players.len()));
    let location = &locations[rng.usize(..locations.len())];
    let mut roles = repo.roles(location).to_vec();
    rng.shuffle(&mut roles);
    let first = players[first_player_index].clone();
    let spy = players.remove(spy_index);
    let player_roles = roles
        .into_iter()
        .cycle()
        .zip(players.into_iter())
        .map(|(role, player)| (player, role))
        .collect::<HashMap<_, _>>();
    GameInfo {
        player_roles,
        first,
        spy,
        location: location.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smol;
    use std::{collections::HashSet, hash::Hash};

    fn to_set<T: Eq + Hash>(vec: Vec<T>) -> HashSet<T> {
        vec.into_iter().collect()
    }

    #[test]
    fn assign_roles_properties() {
        let repo = SpyfallRepo::new();
        let players = (b'a'..=b'z')
            .into_iter()
            .map(char::from)
            .map(String::from)
            .collect::<Vec<_>>();
        let rng = Rng::new();

        let game_info = assign_roles(players.clone(), &repo, &rng);
        assert!(game_info
            .player_roles
            .keys()
            .find(|non_spy| **non_spy == game_info.spy)
            .is_none());
    }

    #[test]
    fn enter_leave_room_deletes_room() {
        smol::block_on(async {
            let player_name = "Ahab".to_string();
            let (client_tx, client_rx) = channel::bounded(1);
            let (broker_tx, broker_rx) = channel::unbounded::<ClientMsg>();
            let broker_task = smol::spawn(async {
                // at the end, the table should be empty
                let table = broker_actor(broker_rx).await.unwrap();
                assert_eq!(table, RoomTable::new());
            });
            let join_msg = ClientMsg::Join(
                Join {
                    name: player_name.clone(),
                    room: None,
                },
                client_tx,
            );
            broker_tx.send(join_msg).await.unwrap();
            let (Connected { room_id, players }, _) = client_rx.recv().await.unwrap().unwrap();
            assert_eq!(players, vec![player_name.clone()]);
            let leave_msg = ClientMsg::Room(RoomMsg::Leave {
                room: room_id,
                name: player_name,
            });
            broker_tx.send(leave_msg).await.unwrap();
            // drop the sending channel so the broker ends
            drop(broker_tx);

            broker_task.await;
        })
    }

    #[test]
    fn two_players_in_room_cant_start_game() {
        smol::block_on(async {
            let player_one = "Ahab".to_string();
            let player_two = "Ishmael".to_string();
            let (client_tx, client_rx) = channel::bounded(1);
            let (broker_tx, broker_rx) = channel::unbounded::<ClientMsg>();
            let broker_task = smol::spawn(broker_actor(broker_rx));
            let join_msg = ClientMsg::Join(
                Join {
                    name: player_one.clone(),
                    room: None,
                },
                client_tx,
            );
            broker_tx.send(join_msg).await.unwrap();
            let (Connected { room_id, players }, player_one_broker_stream) =
                client_rx.recv().await.unwrap().unwrap();
            assert_eq!(players, vec![player_one.clone()]);
            let (client_tx, client_rx) = channel::bounded(1);
            let snd_msg = ClientMsg::Join(
                Join {
                    name: player_two.clone(),
                    room: Some(room_id.clone()),
                },
                client_tx,
            );
            broker_tx.send(snd_msg).await.unwrap();
            let (Connected { room_id, players }, player_two_broker_stream) =
                client_rx.recv().await.unwrap().unwrap();
            assert_eq!(
                to_set(players),
                to_set(vec![player_one.clone(), player_two.clone()])
            );
            assert_eq!(
                player_one_broker_stream.recv().await.unwrap(),
                BrokerMsg::Join(Arc::from(player_two.clone()))
            );

            broker_tx
                .send(ClientMsg::Room(RoomMsg::Start {
                    room: room_id.clone(),
                }))
                .await
                .unwrap();
            for chan in &[player_one_broker_stream, player_two_broker_stream] {
                assert_eq!(chan.recv().await.unwrap(), BrokerMsg::NotEnoughPlayers);
            }

            // drop the sending channel so the broker ends
            drop(broker_tx);

            broker_task.await.unwrap();
        })
    }
}
