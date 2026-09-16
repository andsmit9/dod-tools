//! Demo analyzer that runs in a terminal and produces text output.

#[cfg(not(target_arch = "wasm32"))]
use analysis::{Analysis, MortalityState, Round, SteamId, Team, Mortality};
#[cfg(not(target_arch = "wasm32"))]
use clap::{Parser, Subcommand, ValueEnum};
#[cfg(not(target_arch = "wasm32"))]
use humantime::{format_duration, format_rfc3339_seconds};
#[cfg(not(target_arch = "wasm32"))]
use native::{FileInfo, run_analyzer};
#[cfg(not(target_arch = "wasm32"))]
use native::patch::{patch_demo_highlights, PatchOptions};
#[cfg(not(target_arch = "wasm32"))]
use serde_json::{Value, json};
#[cfg(not(target_arch = "wasm32"))]
use std::fmt::{Display, Formatter};
#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;
#[cfg(not(target_arch = "wasm32"))]
use std::time::{Duration, SystemTime};
#[cfg(not(target_arch = "wasm32"))]
use tabled::{builder::Builder, settings::Style};

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    let args = Args::parse();

    if let Some(command) = args.command {
        match command {
            Commands::Analyze { demo_paths, output_format } => {
                run_analyze_subcommand(demo_paths, output_format);
            }
            Commands::PatchStreak {
                input,
                output,
                player,
                weapon,
                min_streak,
                quit,
                fast_forward_speed,
                initial_delay,
                pre_record_buffer,
                record_start_lead,
                record_stop_trail,
                post_record_buffer,
            } => {
                if let Err(e) = run_patch_streak_subcommand(
                    input,
                    output,
                    player,
                    weapon,
                    min_streak,
                    quit,
                    fast_forward_speed,
                    initial_delay,
                    pre_record_buffer,
                    record_start_lead,
                    record_stop_trail,
                    post_record_buffer,
                ) {
                    eprintln!("Error: {}", e);
                    std::process::exit(1);
                }
            }
        }
    } else {
        if args.demo_paths.is_empty() {
            eprintln!("Error: Please specify at least one demo file to analyze, or use a subcommand.");
            std::process::exit(1);
        }
        run_analyze_subcommand(args.demo_paths, args.output_format);
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn run_analyze_subcommand(demo_paths: Vec<PathBuf>, output_format: OutputFormat) {
    let mut analyses = vec![];
    for p in &demo_paths {
        match run_analyzer(p) {
            Ok(res) => analyses.push(res),
            Err(e) => {
                eprintln!("Error analyzing {}: {}", p.display(), e);
            }
        }
    }

    match output_format {
        OutputFormat::Json => println!("{}", Json::from_iter(analyses)),

        OutputFormat::Markdown => analyses.into_iter().map(Markdown::from).for_each(|output| {
            println!("{output}");
        }),
    };
}

#[cfg(not(target_arch = "wasm32"))]
fn run_patch_streak_subcommand(
    input: PathBuf,
    output: PathBuf,
    player_query: String,
    weapon_query: Option<String>,
    min_streak: usize,
    quit: bool,
    fast_forward_speed: f32,
    initial_delay: f32,
    pre_record_buffer: f32,
    record_start_lead: f32,
    record_stop_trail: f32,
    post_record_buffer: f32,
) -> Result<(), String> {
    println!("Analyzing input demo: {}", input.display());
    let (_file_info, analysis) = run_analyzer(&input)?;

    // Find player
    let target_player = analysis.state.players.iter().find(|p| {
        let steam_id = SteamId::try_from(&p.id)
            .map(|s| s.to_string())
            .unwrap_or_default();
        p.id.to_string() == player_query
            || steam_id == player_query
            || p.name.to_lowercase() == player_query.to_lowercase()
    });

    let player = match target_player {
        Some(p) => p,
        None => {
            return Err(format!(
                "Could not find player matching query '{}'. Available players: {}",
                player_query,
                analysis.state.players.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")
            ));
        }
    };

    println!("Found player: {} ({})", player.name, player.id);

    // Collect killstreaks matching weapon & count filters
    let mut intervals = vec![];
    for streak in &player.kill_streaks {
        if streak.kills.len() < min_streak {
            continue;
        }

        // If weapon query is provided, check if any kill in the streak matches
        if let Some(ref w_query) = weapon_query {
            let matched = streak.kills.iter().any(|(_, weapon, _)| {
                format!("{:?}", weapon).to_lowercase().contains(&w_query.to_lowercase())
            });
            if !matched {
                continue;
            }
        }

        if let (Some(first_kill), Some(last_kill)) = (streak.kills.first(), streak.kills.last()) {
            let start_time = first_kill.0.real_offset.as_secs_f32();
            let stop_time = last_kill.0.real_offset.as_secs_f32();
            intervals.push((start_time, stop_time));
            println!(
                "Selected Streak: {} kills starting at {:.2}s, ending at {:.2}s",
                streak.kills.len(),
                start_time,
                stop_time
            );
        }
    }

    if intervals.is_empty() {
        return Err("No killstreaks found matching the criteria.".to_string());
    }

    println!("Reading raw demo bytes...");
    let demo_bytes = std::fs::read(&input)
        .map_err(|e| format!("Failed to read input demo file: {}", e))?;

    let hltv_spec_player = if analysis.demo_info.demo_type == "HLTV" {
        Some(player.name.clone())
    } else {
        None
    };

    let player_deaths = player.mortality.iter()
        .filter(|change| matches!(change.mortality(), Mortality::Dead))
        .map(|change| change.time().real_offset.as_secs_f32())
        .collect::<Vec<_>>();

    let options = PatchOptions {
        exit_on_finish: quit,
        init_commands: vec![],
        custom_commands: vec![],
        fast_forward_speed: Some(fast_forward_speed),
        hltv_spec_player,
        initial_delay: Some(initial_delay),
        pre_record_buffer: Some(pre_record_buffer),
        record_start_lead: Some(record_start_lead),
        record_stop_trail: Some(record_stop_trail),
        post_record_buffer: Some(post_record_buffer),
        player_deaths: Some(player_deaths),
    };

    println!("Patching demo highlights...");
    let patched_bytes = patch_demo_highlights(&demo_bytes, &intervals, &options)?;

    println!("Writing patched demo to: {}", output.display());
    std::fs::write(&output, patched_bytes)
        .map_err(|e| format!("Failed to write patched demo file: {}", e))?;

    println!("Successfully exported patched demo!");
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Parser)]
#[command(version)]
struct Args {
    #[command(subcommand)]
    command: Option<Commands>,

    /// List of paths to demo files
    #[arg(value_name = "DEMO_PATHS", num_args = 0..)]
    demo_paths: Vec<PathBuf>,

    /// The kind of string output to produce from an analysis
    #[arg(long, value_enum, default_value_t = OutputFormat::Markdown)]
    output_format: OutputFormat,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Subcommand)]
enum Commands {
    /// Analyze one or more demos
    Analyze {
        /// List of paths to demo files
        demo_paths: Vec<PathBuf>,

        /// The kind of string output to produce from an analysis
        #[arg(long, value_enum, default_value_t = OutputFormat::Markdown)]
        output_format: OutputFormat,
    },
    /// Export patched demo capturing player killstreaks
    PatchStreak {
        /// Input demo file path
        #[arg(short, long)]
        input: PathBuf,

        /// Output demo file path
        #[arg(short, long)]
        output: PathBuf,

        /// Filter by player name or SteamID / GlobalID
        #[arg(short, long)]
        player: String,

        /// Filter by weapon (case-insensitive substring, e.g. "k98", "garand")
        #[arg(short, long)]
        weapon: Option<String>,

        /// Minimum killstreak count (default: 3)
        #[arg(short, long, default_value_t = 3)]
        min_streak: usize,

        /// Automatically exit the game 0.5s after playback ends
        #[arg(short, long)]
        quit: bool,

        /// Fast-forward speed multiplier (default: 0.2)
        #[arg(short = 'f', long, default_value_t = 0.2)]
        fast_forward_speed: f32,

        /// Initial delay at normal speed before fast-forwarding in seconds (default: 3.0)
        #[arg(long, default_value_t = 3.0)]
        initial_delay: f32,

        /// Time before killstreak to normalize speed in seconds (default: 6.0)
        #[arg(long, default_value_t = 6.0)]
        pre_record_buffer: f32,

        /// Time before first kill of a streak to start recording in seconds (default: 2.0)
        #[arg(long, default_value_t = 2.0)]
        record_start_lead: f32,

        /// Time after last kill of a streak to stop recording in seconds (default: 2.0)
        #[arg(long, default_value_t = 2.0)]
        record_stop_trail: f32,

        /// Time after last kill of a streak to resume fast-forwarding in seconds (default: 4.0)
        #[arg(long, default_value_t = 4.0)]
        post_record_buffer: f32,
    },
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug, ValueEnum)]
enum OutputFormat {
    /// Markdown document best used in combination with a Markdown renderer
    Markdown,

    /// JSON string for automated tools or custom visualization
    Json,
}

#[cfg(not(target_arch = "wasm32"))]
type AnalyzerOutput = (FileInfo, Analysis);

#[cfg(not(target_arch = "wasm32"))]
struct Json(Value);

#[cfg(not(target_arch = "wasm32"))]
impl FromIterator<AnalyzerOutput> for Json {
    fn from_iter<T: IntoIterator<Item = AnalyzerOutput>>(iter: T) -> Self {
        let analyses = iter.into_iter();

        let json = analyses.fold(vec![], |mut acc, (file, analysis)| {
            let players = analysis
                .state
                .players
                .iter()
                .map(|player| {
                    let id = SteamId::try_from(&player.id)
                        .map(|steam_id| steam_id.to_string())
                        .ok()
                        .unwrap_or(player.id.to_string());

                    json!({
                        "id": id,
                        "name": player.name,
                        "team": player.team.clone().map(|t| format!("{t:?}").to_lowercase()),
                        "score": player.stats.0,
                        "kills": player.stats.1,
                        "deaths": player.stats.2,
                        "lifespan": json!({
                            "avg": format_duration(player.avg_lifespan()).to_string(),
                            "min": format_duration(player.min_lifespan()).to_string(),
                            "max": format_duration(player.max_lifespan()).to_string(),
                        })
                    })
                })
                .collect::<Vec<_>>();

            acc.push(json!({
                "file": file.path,

                "teams": {
                    "allies": if analysis.state.allies_are_british {
                        analysis.state.team_scores.get_team_score(Team::British)
                    } else {
                        analysis.state.team_scores.get_team_score(Team::Allies)
                    },
                    "axis": analysis.state.team_scores.get_team_score(Team::Axis),
                },

                "players": players,
            }));

            acc
        });

        json!(json).into()
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl From<Value> for Json {
    fn from(value: Value) -> Self {
        Self(value)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Display for Json {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let str = serde_json::to_string_pretty(&self.0).map_err(|_| std::fmt::Error)?;

        f.write_str(&str)
    }
}

#[cfg(not(target_arch = "wasm32"))]
struct Markdown(FileInfo, Analysis);

#[cfg(not(target_arch = "wasm32"))]
impl From<AnalyzerOutput> for Markdown {
    fn from(value: AnalyzerOutput) -> Self {
        Self(value.0, value.1)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Markdown {
    fn md_escape(str: &str) -> String {
        str.replace("|", r"\|")
            .replace("_", r"\_")
            .replace("*", r"\*")
            .replace("[", r"\[")
            .replace("]", r"\]")
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Display for Markdown {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        // Header section
        {
            let file_name = &self.0.name;
            let map_name = &self.1.demo_info.map_name;
            writeln!(f, "# Summary: {file_name} on {map_name}\n")?;

            let file_path = &self.0.path;
            writeln!(f, "- File path: `{file_path}`")?;
            let file_created_at = format_rfc3339_seconds(self.0.created_at);
            writeln!(f, "- File created at: {file_created_at}")?;
            let demo_protocol = &self.1.demo_info.demo_protocol;
            writeln!(f, "- Demo protocol: {demo_protocol}")?;
            let network_protocol = &self.1.demo_info.network_protocol;
            writeln!(f, "- Network protocol: {network_protocol}")?;
            let app_version = env!("CARGO_PKG_VERSION");
            writeln!(f, "- Analyzer version: {app_version}")?;
            let report_created_at = format_rfc3339_seconds(SystemTime::now());
            writeln!(f, "- Report created at: {report_created_at}")?;
        }

        writeln!(f)?;

        // Player scoreboard section
        {
            let mut table_builder = Builder::default();
            table_builder.push_record([
                "ID",
                "Name",
                "Team",
                "Class",
                "Score",
                "Kills",
                "Deaths",
                "Avg. Life",
                "Min. Life",
                "Max. Life",
            ]);

            for player in &self.1.state.players {
                table_builder.push_record([
                    player.id.to_string(),
                    Self::md_escape(&player.name),
                    match &player.team {
                        None => "Unknown",
                        Some(Team::Allies) => "Allies",
                        Some(Team::British) => "British",
                        Some(Team::Axis) => "Axis",
                        Some(Team::Spectators) => "Spectators",
                        Some(Team::Unassigned) => "Unassigned",
                    }
                    .to_string(),
                    match &player.class {
                        None => "Unknown".to_string(),
                        Some(x) => format!("{x:?}"),
                    },
                    player.stats.0.to_string(),
                    player.stats.1.to_string(),
                    player.stats.2.to_string(),
                    format_duration(player.avg_lifespan()).to_string(),
                    format_duration(player.min_lifespan()).to_string(),
                    format_duration(player.max_lifespan()).to_string(),
                ]);
            }

            let (allies_score, axis_score) = (
                if self.1.state.allies_are_british {
                    self.1.state.team_scores.get_team_score(Team::British)
                } else {
                    self.1.state.team_scores.get_team_score(Team::Allies)
                },
                self.1.state.team_scores.get_team_score(Team::Axis),
            );

            let allies_name = if self.1.state.allies_are_british { "British" } else { "Allies" };

            let match_result_fragment = format!(
                ": {} ({}) {} Axis ({})",
                allies_name,
                allies_score,
                if allies_score > axis_score { ">" } else { "<" },
                axis_score
            );

            writeln!(f, "## Scoreboard{match_result_fragment}\n")?;

            let mut table = table_builder.build();
            table.with(Style::markdown());

            writeln!(f, "{table}")?;
        }

        writeln!(f)?;

        // Rounds section
        {
            let mut table_builder = Builder::default();
            table_builder.push_record([
                "Round",
                "Start Time",
                "Duration",
                "Winner",
                "Kills by Winner",
            ]);

            for (i, round) in self.1.state.rounds.iter().enumerate() {
                if let Round::Completed {
                    start_time,
                    end_time,
                    winner_stats,
                } = round
                {
                    let duration = Duration::new((end_time - start_time).as_secs(), 0);
                    let start_time = Duration::new(start_time.viewdemo_offset.as_secs(), 0);

                    table_builder.push_record([
                        (i + 1).to_string(),
                        format_duration(start_time).to_string(),
                        format_duration(duration).to_string(),
                        if let Some((winner, _)) = winner_stats {
                            format!("{winner:?}")
                        } else {
                            String::new()
                        },
                        if let Some((_, kills)) = winner_stats {
                            kills.to_string()
                        } else {
                            String::new()
                        },
                    ]);
                }
            }

            writeln!(f, "## Rounds\n")?;

            let mut table = table_builder.build();
            table.with(Style::markdown());

            writeln!(f, "{table}")?;
        }

        writeln!(f)?;

        // Team weapon breakdowns
        {
            writeln!(f, "## Team Weapon Breakdowns\n")?;

            let mut allies_breakdown = std::collections::HashMap::new();
            let mut british_breakdown = std::collections::HashMap::new();
            let mut axis_breakdown = std::collections::HashMap::new();

            for player in &self.1.state.players {
                if let Some(team) = &player.team {
                    let target_map = match team {
                        Team::Allies => Some(&mut allies_breakdown),
                        Team::British => Some(&mut british_breakdown),
                        Team::Axis => Some(&mut axis_breakdown),
                        _ => None,
                    };

                    if let Some(target_map) = target_map {
                        for (weapon, (kills, teamkills)) in &player.weapon_breakdown {
                            let entry = target_map.entry(weapon.clone()).or_insert((0, 0));
                            entry.0 += kills;
                            entry.1 += teamkills;
                        }
                    }
                }
            }

            for (team_name, breakdown) in [
                ("Allies", allies_breakdown),
                ("British", british_breakdown),
                ("Axis", axis_breakdown),
            ] {
                if breakdown.is_empty() {
                    continue;
                }

                writeln!(f, "### {team_name}\n")?;

                let mut table_builder = Builder::default();
                table_builder.push_record(["Weapon", "Kills", "Team Kills"]);

                let mut breakdown_vec = Vec::from_iter(breakdown);
                breakdown_vec.sort_by(|(_, l), (_, r)| l.cmp(r).reverse());

                for (weapon, (kills, teamkills)) in breakdown_vec {
                    table_builder.push_record([
                        format!("{weapon:?}"),
                        kills.to_string(),
                        teamkills.to_string(),
                    ]);
                }

                let mut table = table_builder.build();
                table.with(Style::markdown());

                writeln!(f, "{table}\n")?;
            }
        }

        writeln!(f)?;

        // Individual player summaries
        {
            writeln!(f, "## Player Summaries\n")?;

            for player in &self.1.state.players {
                writeln!(f, "### {}\n", Self::md_escape(&player.name))?;

                // Kills per weapon section
                writeln!(f, "#### Weapon Breakdown\n")?;

                let mut table_builder = Builder::default();
                table_builder.push_record(["Weapon", "Kills", "Team Kills"]);

                for (weapon, (kills, teamkills)) in player.weapon_breakdown.iter() {
                    table_builder.push_record([
                        format!("{weapon:?}"),
                        kills.to_string(),
                        teamkills.to_string(),
                    ]);
                }

                let mut table = table_builder.build();
                table.with(Style::markdown());

                writeln!(f, "{table}\n")?;

                // Kill streaks section
                writeln!(f, "#### Kill Streaks\n")?;

                let mut table_builder = Builder::default();
                table_builder.push_record([
                    "Wave",
                    "Total Kills",
                    "Start Time",
                    "Duration",
                    "Weapons Used",
                ]);

                for (wave, kill_streak) in player.kill_streaks.iter().enumerate() {
                    if let (Some((start_time, _, _)), Some((end_time, _, _))) =
                        (kill_streak.kills.first(), kill_streak.kills.last())
                    {
                        let start_time_offset =
                            Duration::new(start_time.viewdemo_offset.as_secs(), 0);
                        let streak_duration = Duration::new((end_time - start_time).as_secs(), 0);

                        let mut grouped = Vec::new();
                        for (_, weapon, _) in &kill_streak.kills {
                            let name = format!("{weapon:?}");
                            if let Some((last_name, count)) = grouped.last_mut()
                                && *last_name == name {
                                    *count += 1;
                                    continue;
                                }
                            grouped.push((name, 1));
                        }
                        let weapons_used = grouped
                            .into_iter()
                            .map(|(name, count)| {
                                if count > 1 {
                                    format!("{} x{}", name, count)
                                } else {
                                    name
                                }
                            })
                            .collect::<Vec<_>>()
                            .join(", ");

                        table_builder.push_record([
                            (wave + 1).to_string(),
                            kill_streak.kills.len().to_string(),
                            format_duration(start_time_offset).to_string(),
                            format_duration(streak_duration).to_string(),
                            weapons_used,
                        ]);
                    }
                }

                let mut table = table_builder.build();
                table.with(Style::markdown());

                writeln!(f, "{table}\n")?;
            }
        }

        Ok(())
    }
}

#[cfg(target_arch = "wasm32")]
fn main() {}
