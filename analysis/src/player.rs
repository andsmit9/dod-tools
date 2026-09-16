use crate::{AnalyzerEvent, AnalyzerState, kill::KillStreak, mortality::MortalityChange};
use dem::types::EngineMessage;
use dod::{Class, Team, Weapon};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::hash::{Hash, Hasher};

#[derive(Clone, Debug, Eq, Hash, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PlayerGlobalId(String);

impl Display for PlayerGlobalId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Player {
    pub id: PlayerGlobalId,
    pub connection: Connection,
    pub name: String,
    pub team: Option<Team>,
    pub class: Option<Class>,
    pub stats: (i32, i32, i32),
    pub session_stats: (i32, i32, i32),
    pub accumulated_stats: (i32, i32, i32),
    pub needs_reconnect_sync: bool,
    pub stats_seeded: bool,
    pub has_pre_demo_activity: bool,
    pub has_reconnected: bool,
    pub kill_streaks: Vec<KillStreak>,
    pub weapon_breakdown: HashMap<Weapon, (u32, u32)>,
    pub mortality: Vec<MortalityChange>,
}

impl Hash for Player {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state)
    }
}

impl PartialEq for Player {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for Player {}

impl Player {
    fn new(id: PlayerGlobalId) -> Self {
        Self {
            connection: Connection::Disconnected,
            name: String::new(),
            id,
            team: None,
            class: None,
            stats: (0, 0, 0),
            session_stats: (0, 0, 0),
            accumulated_stats: (0, 0, 0),
            needs_reconnect_sync: false,
            stats_seeded: false,
            has_pre_demo_activity: false,
            has_reconnected: false,
            kill_streaks: vec![],
            weapon_breakdown: HashMap::new(),
            mortality: vec![],
        }
    }

    fn with_connection(&mut self, connection: Connection) -> &mut Self {
        self.connection = connection;
        self
    }

    fn with_name(&mut self, name: impl ToString) -> &mut Self {
        self.name = name.to_string();
        self
    }

    fn with_team(&mut self, team: Option<Team>) -> &mut Self {
        self.team = team;
        self
    }

    pub fn update_session_stats(&mut self, score: i32, kills: i32, deaths: i32) {
        let new_stats = (score, kills, deaths);
        if !self.stats_seeded {
            self.stats_seeded = true;
            if score > 0 || kills > 0 || deaths > 0 {
                self.has_pre_demo_activity = true;
            }
        }
        if self.needs_reconnect_sync {
            self.needs_reconnect_sync = false;
            // Detect if stats were reset (drop detected in kills or deaths)
            if kills < self.session_stats.1 || deaths < self.session_stats.2 {
                self.accumulated_stats.0 += self.session_stats.0;
                self.accumulated_stats.1 += self.session_stats.1;
                self.accumulated_stats.2 += self.session_stats.2;
                self.has_reconnected = true;
            }
        }

        self.session_stats = new_stats;
        self.stats = (
            self.accumulated_stats.0 + self.session_stats.0,
            self.accumulated_stats.1 + self.session_stats.1,
            self.accumulated_stats.2 + self.session_stats.2,
        );
    }

    pub fn update_frags(&mut self, frags: i32) {
        let score = self.session_stats.0;
        let deaths = self.session_stats.2;
        self.update_session_stats(score, frags, deaths);
    }

    pub fn update_obj_score(&mut self, score: i32) {
        let frags = self.session_stats.1;
        let deaths = self.session_stats.2;
        self.update_session_stats(score, frags, deaths);
    }

    #[cfg(test)]
    pub fn new_mock(client_id: u8, name: &str) -> Self {
        Self {
            id: PlayerGlobalId(format!("STEAM_0:0:12345:{}", name)),
            connection: Connection::Connected { client_id },
            name: name.to_string(),
            team: None,
            class: None,
            stats: (0, 0, 0),
            session_stats: (0, 0, 0),
            accumulated_stats: (0, 0, 0),
            needs_reconnect_sync: false,
            stats_seeded: false,
            has_pre_demo_activity: false,
            has_reconnected: false,
            kill_streaks: vec![],
            weapon_breakdown: HashMap::new(),
            mortality: vec![],
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SteamId(String);

impl Display for SteamId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<&PlayerGlobalId> for SteamId {
    type Error = std::num::ParseIntError;

    fn try_from(value: &PlayerGlobalId) -> Result<Self, Self::Error> {
        // Standard SteamID64 to SteamID conversion formula
        // Reference: https://developer.valvesoftware.com/wiki/SteamID
        let id64 = value.to_string().parse::<u64>()?;
        let universe = 0; // Public

        let account_id = id64 - 76561197960265728;
        let server_id = if account_id % 2 == 0 { 0 } else { 1 };
        let account_id = (account_id - server_id) / 2;

        let steam_id = format!("STEAM_{}:{}:{}", universe, server_id, account_id);

        Ok(SteamId(steam_id))
    }
}

/// Represents whether a [Player] is connected to the server.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub enum Connection {
    /// Player is currently connected to the server.
    Connected {
        /// Identifier assigned by the server that represents the [Player]'s connection.
        client_id: u8,
    },

    Disconnected,
}

pub fn use_player_updates(state: &mut AnalyzerState, event: &AnalyzerEvent) {
    let svc_update_user_info = match event {
        AnalyzerEvent::EngineMessage(EngineMessage::SvcUpdateUserInfo(msg)) => Some(msg),
        _ => None,
    };

    if let Some(svc_update_user_info) = svc_update_user_info {
        let fields = svc_update_user_info
            .user_info
            .to_str()
            .map(|s| s.trim_matches(['\0', '\\']).split("\\").collect::<Vec<_>>())
            .unwrap_or_default()
            .as_chunks::<2>()
            .0
            .iter()
            .fold(HashMap::new(), |mut map, [key, value]| {
                map.insert(*key, *value);

                map
            });

        // Missing fields indicates that the user has disconnected, so we only update their
        // connection status and preserve the last known details.
        if fields.is_empty() {
            let player = state.find_player_by_client_index_mut(svc_update_user_info.index);

            if let Some(disconnected_player) = player {
                disconnected_player.connection = Connection::Disconnected;
                disconnected_player.needs_reconnect_sync = true;
                return;
            }
        }

        // HLTV clients have this field set to 1. We can skip them because whatever slot it occupies
        // will never be referenced by game events, unless someone else takes that slot.
        if let Some(&"1") = fields.get("*hltv") {
            if let Some(name) = fields.get("name") {
                state.hltv_name = Some(name.to_string());
            }
            return;
        }

        let id = fields
            .get("*sid")
            .map(|s| s.to_string())
            .or_else(|| {
                // When present, *fid still seems unique to players across demos. Can it be mapped
                // to a SteamID64?
                //
                // ("93",      76561197960269086, "STEAM_0:0:1679"),  // Las1k
                // ("117",     76561197960269100, "STEAM_0:0:1686"),  // Money-B
                // ("100",     76561197960269104, "STEAM_0:0:1688"),  // scrd?
                // ("2761379", 76561197960366973, "STEAM_0:1:50622"), // jdub
                fields.get("*fid").map(|fid| format!("PLAYER_{fid}"))
            })
            .or_else(|| Some(format!("CONNECTION_{}", svc_update_user_info.id)))
            .map(PlayerGlobalId)
            .unwrap_or_else(|| {
                panic!(
                    "Could not resolve a global id for player {} in slot {}",
                    svc_update_user_info.id, svc_update_user_info.index
                )
            });

        let player_name = fields
            .get("name")
            .map(|x| x.to_string())
            .unwrap_or(format!("Player {}", svc_update_user_info.id));

        // Make sure a record of this player exists first
        if state.find_player_by_id(&id).is_none() {
            let insert_id = id.clone();
            let new_player = Player::new(insert_id);

            state.players.push(new_player);
        };

        // Flush any existing player from this slot
        if let Some(player_in_slot) =
            state.find_player_by_client_index_mut(svc_update_user_info.index)
        {
            player_in_slot.with_connection(Connection::Disconnected);
            player_in_slot.needs_reconnect_sync = true;
        }

        // Find the player from the message, and assign it to the slot
        if let Some(player) = state.find_player_by_id_mut(&id) {
            player
                .with_connection(Connection::Connected {
                    client_id: svc_update_user_info.index,
                })
                .with_name(player_name)
                .with_team(
                    fields
                        .get("team")
                        .and_then(|team| Team::try_from(*team).ok()),
                );
            player.needs_reconnect_sync = true;
        }
    }
}
