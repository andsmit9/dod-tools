use crate::time::GameTime;
use crate::{AnalyzerEvent, AnalyzerState};
use dod::{RoundState, Team, UserMessage};

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub enum Round {
    Active {
        allies_kills: u32,
        axis_kills: u32,
        start_time: GameTime,
    },

    Completed {
        start_time: GameTime,
        end_time: GameTime,
        winner_stats: Option<(Team, u32)>,
    },
}

pub fn use_rounds_updates(state: &mut AnalyzerState, event: &AnalyzerEvent) {
    match event {
        AnalyzerEvent::Initialization => {
            if state.rounds.is_empty() {
                state.rounds.push(Round::Active {
                    allies_kills: 0,
                    axis_kills: 0,
                    start_time: state.current_time.clone(),
                });
            }
        }

        AnalyzerEvent::Finalization => {
            if let Some(Round::Active { start_time, .. }) = state.rounds.pop() {
                state.rounds.push(Round::Completed {
                    start_time: start_time.clone(),
                    end_time: state.current_time.clone(),
                    winner_stats: None,
                });
            }
        }

        AnalyzerEvent::UserMessage(UserMessage::RoundState(round_state)) => {
            match round_state {
                RoundState::Reset => {
                    state.rounds.push(Round::Active {
                        allies_kills: 0,
                        axis_kills: 0,
                        start_time: state.current_time.clone(),
                    });
                }

                RoundState::AlliesWin | RoundState::AxisWin | RoundState::Draw => {
                    if let Some(last_round) = state.rounds.last()
                        && matches!(last_round, Round::Active { .. })
                            && let Some(Round::Active {
                                start_time,
                                allies_kills,
                                axis_kills,
                            }) = state.rounds.pop()
                            {
                                let winner_stats = match round_state {
                                    RoundState::AlliesWin => {
                                        let team = if state.allies_are_british { Team::British } else { Team::Allies };
                                        Some((team, allies_kills))
                                    }
                                    RoundState::AxisWin => Some((Team::Axis, axis_kills)),
                                    _ => None,
                                };

                                let completed_round = Round::Completed {
                                    start_time,
                                    end_time: state.current_time.clone(),
                                    winner_stats,
                                };

                                state.rounds.push(completed_round);
                            }
                }

                _ => {}
            };
        }

        AnalyzerEvent::UserMessage(UserMessage::DeathMsg(death_msg)) => {
            let killer = if death_msg.killer_client_index > 0 {
                state.find_player_by_client_index(death_msg.killer_client_index - 1)
            } else {
                None
            };
            let victim = state.find_player_by_client_index(death_msg.victim_client_index - 1);

            let kill_info = match (killer, victim) {
                (Some(killer), Some(victim)) => Some((
                    killer.team.clone(),
                    killer.team.is_some() && killer.team == victim.team,
                )),
                _ => None,
            };

            if let (
                Some(Round::Active {
                    allies_kills,
                    axis_kills,
                    ..
                }),
                Some((team, is_teamkill)),
            ) = (state.rounds.last_mut(), kill_info)
            {
                if is_teamkill {
                    return;
                }

                if let Some(Team::Allies) | Some(Team::British) = team {
                    *allies_kills += 1;
                } else if let Some(Team::Axis) = team {
                    *axis_kills += 1;
                }
            }
        }

        _ => {}
    };
}
