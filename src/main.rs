use std::collections::HashMap;

use poise::serenity_prelude::{
    self as serenity, Context, CreateEmbed, CreateMessage, EventHandler, Member, Mentionable,
    async_trait,
};
use serde::{Deserialize, Serialize};

const COLOR_SUCCESS: u32 = 0x57F287; // Green
const COLOR_ERROR: u32 = 0xED4245; // Red
const COLOR_WARNING: u32 = 0xFEE75C; // Yellow
const COLOR_INFO: u32 = 0x5865F2; // Blurple

#[derive(Deserialize, Serialize, Clone)]
struct Config {
    discord_token: String,
    /// mapping of server name to base URL, e.g. {"pvp": "https://db.cm-ss13.com", "pve": "https://pve-db.cm-ss13.com"}
    server_urls: HashMap<String, String>,
    api_token: String,
    guild_id: u64,
    verified_role_id: u64,
    unverified_role_id: u64,
    verification_channel_id: u64,
    verification_instructions: String,
    owner_id: u64,
}

impl Config {
    /// Get the default base URL (uses "pvp")
    fn default_base_url(&self) -> &str {
        self.server_urls
            .get("pvp")
            .map(|s| s.as_str())
            .unwrap_or("")
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct DiscordUserResponse {
    pub source: String,
    pub ckey: String,
    pub discord_id: String,
    pub authentik_username: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct PlayerResponse {
    id: i64,
    ckey: Option<String>,
    last_login: Option<String>,
    is_permabanned: Option<i32>,
    permaban_reason: Option<String>,
    permaban_date: Option<String>,
    permaban_admin_ckey: Option<String>,
    is_time_banned: Option<i32>,
    time_ban_reason: Option<String>,
    time_ban_admin_ckey: Option<String>,
    time_ban_date: Option<String>,
    time_ban_expiration: Option<i64>,
    whitelist_status: Option<String>,
    byond_account_age: Option<String>,
    first_join_date: Option<String>,
    discord_id: Option<String>,
    notes: Option<Vec<NoteResponse>>,
    job_bans: Option<Vec<JobBanResponse>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct NoteResponse {
    id: i64,
    player_id: i64,
    admin_id: i64,
    text: Option<String>,
    date: String,
    is_ban: i32,
    ban_time: Option<i64>,
    is_confidential: i32,
    admin_rank: String,
    note_category: Option<i32>,
    round_id: Option<i32>,
    noted_player_ckey: Option<String>,
    noting_admin_ckey: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct JobBanResponse {
    id: i64,
    text: String,
    role: String,
    expiration: Option<i64>,
    banning_admin_ckey: Option<String>,
}

struct BotData {
    config: Config,
    http_client: reqwest::Client,
}

type Error = Box<dyn std::error::Error + Send + Sync>;
type PoiseContext<'a> = poise::Context<'a, BotData, Error>;

/// Format a single note as structured text
fn format_note(note: &NoteResponse) -> String {
    let category = match note.note_category {
        Some(2) => "Merit",
        Some(3) => "Whitelist",
        _ => "Admin",
    }
    .to_uppercase();

    let note_text = note.text.as_deref().unwrap_or("No text");
    let admin = note.noting_admin_ckey.as_deref().unwrap_or("Unknown");

    let confidential = if note.is_confidential != 0 {
        " [CONFIDENTIAL]"
    } else {
        ""
    };

    let round_info = note
        .round_id
        .map(|id| format!(" (Round {})", id))
        .unwrap_or_default();

    let truncated_text = if note_text.len() > 300 {
        format!("{}...", &note_text[..300])
    } else {
        note_text.to_string()
    };

    format!(
        "**[{}]** {}\n*by {} on {}{}{}*",
        category, truncated_text, admin, note.date, confidential, round_info
    )
}

fn build_notes_embeds(notes: &[&NoteResponse], ckey: &str, server: &str) -> Vec<CreateEmbed> {
    let mut embeds = Vec::new();
    let mut current_description = String::new();
    let total_notes = notes.len();

    const MAX_DESCRIPTION_LEN: usize = 4000;

    for (i, note) in notes.iter().enumerate() {
        let formatted = format_note(note);
        let separator = if current_description.is_empty() {
            String::new()
        } else {
            "\n\n".to_string()
        };

        if current_description.len() + separator.len() + formatted.len() > MAX_DESCRIPTION_LEN {
            let embed = CreateEmbed::new()
                .title(if embeds.is_empty() {
                    format!("Notes for {} ({} total)", ckey, total_notes)
                } else {
                    format!("Notes for {} (continued)", ckey)
                })
                .description(&current_description)
                .footer(serenity::CreateEmbedFooter::new(format!(
                    "Server: {}",
                    server
                )))
                .color(COLOR_INFO);
            embeds.push(embed);
            current_description = formatted;
        } else {
            current_description.push_str(&separator);
            current_description.push_str(&formatted);
        }

        if i == notes.len() - 1 && !current_description.is_empty() {
            let embed = CreateEmbed::new()
                .title(if embeds.is_empty() {
                    format!("Notes for {} ({} total)", ckey, total_notes)
                } else {
                    format!("Notes for {} (continued)", ckey)
                })
                .description(&current_description)
                .footer(serenity::CreateEmbedFooter::new(format!(
                    "Server: {}",
                    server
                )))
                .color(COLOR_INFO);
            embeds.push(embed);
        }
    }

    embeds
}

/// Verify your account against the CM database
#[poise::command(slash_command)]
async fn verify(ctx: PoiseContext<'_>) -> Result<(), Error> {
    let target_user = ctx.author();
    let guild_id = ctx.data().config.guild_id;

    let Some(current_guild_id) = ctx.guild_id() else {
        let embed = CreateEmbed::new()
            .title("Error")
            .description("This command must be run in a server.")
            .color(COLOR_ERROR);
        ctx.send(poise::CreateReply::default().embed(embed)).await?;
        return Ok(());
    };

    if current_guild_id.get() != guild_id {
        let embed = CreateEmbed::new()
            .title("Error")
            .description("This command is not available in this server.")
            .color(COLOR_ERROR);
        ctx.send(poise::CreateReply::default().embed(embed)).await?;
        return Ok(());
    }

    ctx.defer().await?;

    match verify_user(
        &ctx.data().http_client,
        ctx.data().config.default_base_url(),
        &ctx.data().config.api_token,
        target_user.id.get(),
        current_guild_id.get(),
    )
    .await
    {
        Ok(Some(_user_info)) => {
            let member = current_guild_id.member(ctx.http(), target_user.id).await?;
            member
                .add_role(
                    ctx.http(),
                    serenity::RoleId::new(ctx.data().config.verified_role_id),
                )
                .await?;
            member
                .remove_role(
                    ctx.http(),
                    serenity::RoleId::new(ctx.data().config.unverified_role_id),
                )
                .await?;
            let embed = CreateEmbed::new()
                .title("Verification Successful")
                .description(format!("{} has been verified.", target_user.name))
                .color(COLOR_SUCCESS);
            ctx.send(poise::CreateReply::default().embed(embed)).await?;
        }
        Ok(None) => {
            let embed = CreateEmbed::new()
                .title("Verification Failed")
                .description(format!(
                    "{} could not be verified. \n\n**How to verify:**\n{}",
                    target_user.name,
                    ctx.data().config.verification_instructions
                ))
                .color(COLOR_WARNING);
            ctx.send(poise::CreateReply::default().embed(embed)).await?;
        }
        Err(e) => {
            let embed = CreateEmbed::new()
                .title("Error")
                .description(format!("Error during verification: {}", e))
                .color(COLOR_ERROR);
            ctx.send(poise::CreateReply::default().embed(embed)).await?;
        }
    }

    Ok(())
}

/// Verify a specific user's Discord account against the CM database (moderators only)
#[poise::command(
    slash_command,
    required_permissions = "MANAGE_MESSAGES",
    default_member_permissions = "MANAGE_MESSAGES"
)]
async fn verify_for(
    ctx: PoiseContext<'_>,
    #[description = "The user to verify"] user: serenity::User,
) -> Result<(), Error> {
    let guild_id = ctx.data().config.guild_id;

    let Some(current_guild_id) = ctx.guild_id() else {
        let embed = CreateEmbed::new()
            .title("Error")
            .description("This command must be run in a server.")
            .color(COLOR_ERROR);
        ctx.send(poise::CreateReply::default().embed(embed)).await?;
        return Ok(());
    };

    if current_guild_id.get() != guild_id {
        let embed = CreateEmbed::new()
            .title("Error")
            .description("This command is not available in this server.")
            .color(COLOR_ERROR);
        ctx.send(poise::CreateReply::default().embed(embed)).await?;
        return Ok(());
    }

    ctx.defer().await?;

    match verify_user(
        &ctx.data().http_client,
        ctx.data().config.default_base_url(),
        &ctx.data().config.api_token,
        user.id.get(),
        current_guild_id.get(),
    )
    .await
    {
        Ok(Some(user_info)) => {
            let member = current_guild_id.member(ctx.http(), user.id).await?;
            member
                .add_role(
                    ctx.http(),
                    serenity::RoleId::new(ctx.data().config.verified_role_id),
                )
                .await?;
            member
                .remove_role(
                    ctx.http(),
                    serenity::RoleId::new(ctx.data().config.unverified_role_id),
                )
                .await?;
            let embed = CreateEmbed::new()
                .title("Verification Successful")
                .description(format!("{} has been verified.", user.name))
                .field("CKEY", &user_info.ckey, true)
                .field("Source", &user_info.source, true)
                .color(COLOR_SUCCESS);
            ctx.send(poise::CreateReply::default().embed(embed)).await?;
        }
        Ok(None) => {
            let embed = CreateEmbed::new()
                .title("Verification Failed")
                .description(format!(
                    "{} could not be verified. No matching account found in the database.",
                    user.name
                ))
                .color(COLOR_WARNING);
            ctx.send(poise::CreateReply::default().embed(embed)).await?;
        }
        Err(e) => {
            let embed = CreateEmbed::new()
                .title("Error")
                .description(format!("Error during verification: {}", e))
                .color(COLOR_ERROR);
            ctx.send(poise::CreateReply::default().embed(embed)).await?;
        }
    }

    Ok(())
}

/// Look up a user's linked ckey (staff only)
#[poise::command(
    slash_command,
    required_permissions = "MANAGE_MESSAGES",
    default_member_permissions = "MANAGE_MESSAGES",
    ephemeral
)]
async fn whois(
    ctx: PoiseContext<'_>,
    #[description = "The user to look up"] user: serenity::User,
) -> Result<(), Error> {
    let guild_id = ctx.data().config.guild_id;

    let Some(current_guild_id) = ctx.guild_id() else {
        let embed = CreateEmbed::new()
            .title("Error")
            .description("This command must be run in a server.")
            .color(COLOR_ERROR);
        ctx.send(poise::CreateReply::default().embed(embed)).await?;
        return Ok(());
    };

    if current_guild_id.get() != guild_id {
        let embed = CreateEmbed::new()
            .title("Error")
            .description("This command is not available in this server.")
            .color(COLOR_ERROR);
        ctx.send(poise::CreateReply::default().embed(embed)).await?;
        return Ok(());
    }

    ctx.defer_ephemeral().await?;

    match lookup_discord_user(
        &ctx.data().http_client,
        ctx.data().config.default_base_url(),
        &ctx.data().config.api_token,
        user.id.get(),
    )
    .await
    {
        Ok(Some(user_info)) => {
            let embed = CreateEmbed::new()
                .title("User Lookup")
                .description(format!("Information for {}", user.name))
                .field("CKEY", &user_info.ckey, true)
                .field("Source", &user_info.source, true)
                .field("Discord ID", &user_info.discord_id, true)
                .color(COLOR_SUCCESS);
            ctx.send(poise::CreateReply::default().embed(embed)).await?;
        }
        Ok(None) => {
            let embed = CreateEmbed::new()
                .title("User Not Found")
                .description(format!(
                    "{} is not linked to any account in the database.",
                    user.name
                ))
                .color(COLOR_WARNING);
            ctx.send(poise::CreateReply::default().embed(embed)).await?;
        }
        Err(e) => {
            let embed = CreateEmbed::new()
                .title("Error")
                .description(format!("Error during lookup: {}", e))
                .color(COLOR_ERROR);
            ctx.send(poise::CreateReply::default().embed(embed)).await?;
        }
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, poise::ChoiceParameter)]
enum Server {
    #[name = "PVP"]
    Pvp,
    #[name = "PVE"]
    Pve,
}

impl Server {
    fn as_key(&self) -> &'static str {
        match self {
            Server::Pvp => "pvp",
            Server::Pve => "pve",
        }
    }
}

/// look up a player's information from the CM database (staff only)
#[poise::command(
    slash_command,
    required_permissions = "MANAGE_MESSAGES",
    default_member_permissions = "MANAGE_MESSAGES"
)]
async fn player(
    ctx: PoiseContext<'_>,
    #[description = "Server to query"] server: Server,
    #[description = "The user to look up (optional)"] user: Option<serenity::User>,
    #[description = "The ckey to look up (optional)"] ckey: Option<String>,
    #[description = "Hide response from others (default: true)"] ephemeral: Option<bool>,
) -> Result<(), Error> {
    let ephemeral = ephemeral.unwrap_or(true);

    if user.is_none() && ckey.is_none() {
        let embed = CreateEmbed::new()
            .title("Error")
            .description("You must provide either a user or a ckey.")
            .color(COLOR_ERROR);
        ctx.send(
            poise::CreateReply::default()
                .ephemeral(ephemeral)
                .embed(embed),
        )
        .await?;
        return Ok(());
    }

    let Some(base_url) = ctx.data().config.server_urls.get(server.as_key()) else {
        let embed = CreateEmbed::new()
            .title("Error")
            .description(format!("Server '{}' is not configured.", server.as_key()))
            .color(COLOR_ERROR);
        ctx.send(
            poise::CreateReply::default()
                .ephemeral(ephemeral)
                .embed(embed),
        )
        .await?;
        return Ok(());
    };

    if ephemeral {
        ctx.defer_ephemeral().await?;
    } else {
        ctx.defer().await?;
    }

    let lookup_ckey: String = if let Some(ckey) = ckey {
        ckey
    } else if let Some(user) = &user {
        match lookup_discord_user(
            &ctx.data().http_client,
            base_url,
            &ctx.data().config.api_token,
            user.id.get(),
        )
        .await?
        {
            Some(discord_user) => discord_user.ckey,
            None => {
                let embed = CreateEmbed::new()
                    .title("User Not Found")
                    .description(format!(
                        "{} is not linked to any account in the {} database.",
                        user.name,
                        server.as_key().to_uppercase()
                    ))
                    .color(COLOR_WARNING);
                ctx.send(
                    poise::CreateReply::default()
                        .ephemeral(ephemeral)
                        .embed(embed),
                )
                .await?;
                return Ok(());
            }
        }
    } else {
        unreachable!()
    };

    match get_player(
        &ctx.data().http_client,
        base_url,
        &ctx.data().config.api_token,
        &lookup_ckey,
    )
    .await?
    {
        Some(player_info) => {
            let ckey_display = player_info.ckey.as_deref().unwrap_or("Unknown");

            let is_permabanned = player_info.is_permabanned.unwrap_or(0) != 0;
            let is_timebanned = player_info.is_time_banned.unwrap_or(0) != 0;

            let embed_color = if is_permabanned {
                COLOR_ERROR
            } else if is_timebanned {
                COLOR_WARNING
            } else {
                COLOR_INFO
            };

            let mut embed = CreateEmbed::new()
                .title(format!("Player: {}", ckey_display))
                .description(format!("Server: **{}**", server.as_key().to_uppercase()))
                .color(embed_color);

            if let Some(first_join) = &player_info.first_join_date {
                embed = embed.field("First Join", first_join, true);
            }
            if let Some(last_login) = &player_info.last_login {
                embed = embed.field("Last Login", last_login, true);
            }
            if let Some(byond_age) = &player_info.byond_account_age {
                embed = embed.field("BYOND Account Age", byond_age, true);
            }
            if let Some(whitelist) = &player_info.whitelist_status {
                embed = embed.field("Whitelist Status", whitelist, true);
            }
            if let Some(discord_id) = &player_info.discord_id {
                embed = embed.field("Discord", format!("<@{}>", discord_id), true);
            }

            if is_permabanned {
                let admin = player_info
                    .permaban_admin_ckey
                    .as_deref()
                    .unwrap_or("adminbot");
                let reason = player_info
                    .permaban_reason
                    .as_deref()
                    .unwrap_or("No reason provided");
                let date = player_info
                    .permaban_date
                    .as_deref()
                    .unwrap_or("Unknown date");

                embed = embed.field(
                    "PERMABANNED",
                    format!("By **{}** on {}\n{}", admin, date, reason),
                    false,
                );
            }

            if is_timebanned {
                let admin = player_info
                    .time_ban_admin_ckey
                    .as_deref()
                    .unwrap_or("adminbot");
                let reason = player_info
                    .time_ban_reason
                    .as_deref()
                    .unwrap_or("No reason provided");
                let date = player_info
                    .time_ban_date
                    .as_deref()
                    .unwrap_or("Unknown date");
                let expiration = player_info
                    .time_ban_expiration
                    .map(|exp| {
                        const BYOND_EPOCH_UNIX: i64 = 946684800; // Unix timestamp for Jan 1 2000
                        let unix_timestamp = BYOND_EPOCH_UNIX + (exp * 60);
                        format!("<t:{}:R>", unix_timestamp)
                    })
                    .unwrap_or_else(|| "Unknown".to_string());

                embed = embed.field(
                    "TIME BANNED",
                    format!(
                        "By **{}** on {}\nExpires: {}\n{}",
                        admin, date, expiration, reason
                    ),
                    false,
                );
            }

            if let Some(notes) = &player_info.notes {
                embed = embed.field("Notes", notes.len().to_string(), true);
            }

            if let Some(job_bans) = &player_info.job_bans
                && !job_bans.is_empty()
            {
                embed = embed.field("Job Bans", job_bans.len().to_string(), true);
            }

            let mut reply = poise::CreateReply::default()
                .ephemeral(ephemeral)
                .embed(embed);

            if let Some(notes) = &player_info.notes
                && !notes.is_empty()
            {
                let mut sorted_notes: Vec<_> = notes.iter().collect();
                sorted_notes.sort_by(|a, b| a.date.cmp(&b.date));

                let notes_embeds = build_notes_embeds(
                    &sorted_notes,
                    ckey_display,
                    &server.as_key().to_uppercase(),
                );

                for notes_embed in notes_embeds {
                    reply = reply.embed(notes_embed);
                }
            }

            ctx.send(reply).await?;
        }
        None => {
            let embed = CreateEmbed::new()
                .title("Player Not Found")
                .description(format!(
                    "No player with ckey '{}' found in the {} database.",
                    lookup_ckey,
                    server.as_key().to_uppercase()
                ))
                .color(COLOR_WARNING);
            ctx.send(
                poise::CreateReply::default()
                    .ephemeral(ephemeral)
                    .embed(embed),
            )
            .await?;
        }
    }

    Ok(())
}

/// Verify all members in the server (owner only)
#[poise::command(slash_command, owners_only)]
async fn verify_all(ctx: PoiseContext<'_>) -> Result<(), Error> {
    let guild_id = ctx.data().config.guild_id;

    let Some(current_guild_id) = ctx.guild_id() else {
        let embed = CreateEmbed::new()
            .title("Error")
            .description("This command must be run in a server.")
            .color(COLOR_ERROR);
        ctx.send(poise::CreateReply::default().embed(embed)).await?;
        return Ok(());
    };

    if current_guild_id.get() != guild_id {
        let embed = CreateEmbed::new()
            .title("Error")
            .description("This command is not available in this server.")
            .color(COLOR_ERROR);
        ctx.send(poise::CreateReply::default().embed(embed)).await?;
        return Ok(());
    }

    ctx.defer().await?;

    let embed = CreateEmbed::new()
        .title("Verification Started")
        .description("Starting bulk verification of all server members. This may take a while...")
        .color(COLOR_WARNING);
    let progress_message = ctx.send(poise::CreateReply::default().embed(embed)).await?;

    let verified_role_id = serenity::RoleId::new(ctx.data().config.verified_role_id);
    let unverified_role_id = serenity::RoleId::new(ctx.data().config.unverified_role_id);

    let mut all_members = Vec::new();
    let mut last_member_id: Option<serenity::UserId> = None;

    loop {
        let members = current_guild_id
            .members(ctx.http(), Some(1000), last_member_id)
            .await?;

        if members.is_empty() {
            break;
        }

        last_member_id = members.last().map(|m| m.user.id);

        all_members.extend(members);

        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }

    let total = all_members.len();
    let mut verified_count = 0u32;
    let mut already_verified = 0u32;
    let mut failed_count = 0u32;
    let mut not_found_count = 0u32;
    let mut processed_count = 0usize;
    let progress_interval = 100;

    for member in all_members {
        if member.user.bot {
            processed_count += 1;
            continue;
        }

        if member.roles.contains(&verified_role_id) {
            already_verified += 1;
            processed_count += 1;
            continue;
        }

        match verify_user(
            &ctx.data().http_client,
            ctx.data().config.default_base_url(),
            &ctx.data().config.api_token,
            member.user.id.get(),
            current_guild_id.get(),
        )
        .await
        {
            Ok(Some(_user_info)) => {
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

                if let Err(e) = member.add_role(ctx.http(), verified_role_id).await {
                    println!("Failed to add role to {}: {}", member.user.name, e);
                    failed_count += 1;
                } else {
                    println!("Verified: {}", member.user.name);
                    verified_count += 1;
                }
            }
            Ok(None) => {
                if !member.roles.contains(&unverified_role_id) {
                    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                    if let Err(e) = member.add_role(ctx.http(), unverified_role_id).await {
                        println!(
                            "Failed to add unverified role to {}: {}",
                            member.user.name, e
                        );
                    }
                }
                not_found_count += 1;
            }
            Err(e) => {
                println!("Error verifying {}: {}", member.user.name, e);
                failed_count += 1;
            }
        }

        processed_count += 1;

        if processed_count % progress_interval == 0 {
            let progress_embed = CreateEmbed::new()
                .title("Verification Progress")
                .description(format!(
                    "Processed {}/{} members ({:.1}%)",
                    processed_count,
                    total,
                    (processed_count as f64 / total as f64) * 100.0
                ))
                .field("Newly Verified", verified_count.to_string(), true)
                .field("Not Found", not_found_count.to_string(), true)
                .field("Failed", failed_count.to_string(), true)
                .color(COLOR_WARNING);

            let _ = progress_message
                .edit(ctx, poise::CreateReply::default().embed(progress_embed))
                .await;
        }
    }

    let embed = CreateEmbed::new()
        .title("Bulk Verification Complete")
        .description(format!("Processed {} members", total))
        .field("Newly Verified", verified_count.to_string(), true)
        .field("Already Verified", already_verified.to_string(), true)
        .field("Not Found", not_found_count.to_string(), true)
        .field("Failed", failed_count.to_string(), true)
        .color(COLOR_SUCCESS);

    progress_message
        .edit(ctx, poise::CreateReply::default().embed(embed))
        .await?;

    Ok(())
}

async fn verify_user(
    http_client: &reqwest::Client,
    base_url: &str,
    api_token: &str,
    discord_id: u64,
    guild_id: u64,
) -> Result<Option<DiscordUserResponse>, Error> {
    let url = format!("{}/api/Discord/Verified/{}/{}", base_url, discord_id, guild_id);

    let response = http_client
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_token))
        .send()
        .await?;

    if response.status().is_success() {
        let user_response: DiscordUserResponse = response.json().await?;
        Ok(Some(user_response))
    } else if response.status() == reqwest::StatusCode::NOT_FOUND {
        Ok(None)
    } else {
        Err(format!("API request failed with status: {}", response.status()).into())
    }
}

async fn lookup_discord_user(
    http_client: &reqwest::Client,
    base_url: &str,
    api_token: &str,
    discord_id: u64,
) -> Result<Option<DiscordUserResponse>, Error> {
    let url = format!("{}/api/Discord/User/{}", base_url, discord_id);

    let response = http_client
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_token))
        .send()
        .await?;

    if response.status().is_success() {
        let user_response: DiscordUserResponse = response.json().await?;
        Ok(Some(user_response))
    } else if response.status() == reqwest::StatusCode::NOT_FOUND {
        Ok(None)
    } else {
        Err(format!("API request failed with status: {}", response.status()).into())
    }
}

async fn get_player(
    http_client: &reqwest::Client,
    base_url: &str,
    api_token: &str,
    ckey: &str,
) -> Result<Option<PlayerResponse>, Error> {
    let url = format!("{}/api/User?ckey={}", base_url, ckey);

    let response = http_client
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_token))
        .send()
        .await?;

    if response.status().is_success() {
        let player_response: PlayerResponse = response.json().await?;
        Ok(Some(player_response))
    } else if response.status() == reqwest::StatusCode::NOT_FOUND {
        Ok(None)
    } else {
        Err(format!("API request failed with status: {}", response.status()).into())
    }
}

struct NewUserHandler {
    config: Config,
    http_client: reqwest::Client,
}

#[async_trait]
impl EventHandler for NewUserHandler {
    async fn guild_member_addition(&self, ctx: Context, new_member: Member) {
        if new_member.guild_id.get() != self.config.guild_id {
            return;
        }

        let discord_id = new_member.user.id.get();

        match verify_user(
            &self.http_client,
            self.config.default_base_url(),
            &self.config.api_token,
            discord_id,
            new_member.guild_id.get(),
        )
        .await
        {
            Ok(Some(user_info)) => {
                if let Err(e) = new_member
                    .add_role(
                        &ctx.http,
                        serenity::RoleId::new(self.config.verified_role_id),
                    )
                    .await
                {
                    eprintln!("Failed to add role to user {}: {}", new_member.user.name, e);
                } else {
                    println!(
                        "Verified and added role to new member: {} (ckey: {})",
                        new_member.user.name, user_info.ckey
                    );
                }
            }
            Ok(None) => {
                println!(
                    "New member {} not found in database, sending verification instructions",
                    new_member.user.name
                );

                if let Err(e) = new_member
                    .add_role(
                        &ctx.http,
                        serenity::RoleId::new(self.config.unverified_role_id),
                    )
                    .await
                {
                    eprintln!(
                        "Failed to add unverified role to user {}: {}",
                        new_member.user.name, e
                    );
                }

                let channel_id = serenity::ChannelId::new(self.config.verification_channel_id);
                let embed = CreateEmbed::new()
                    .title("Verification Required")
                    .description(format!(
                        "Welcome {}! We require you to verify your BYOND account with us before you can join.\n\n{}",
                        new_member.user.mention(),
                        self.config.verification_instructions
                    ))
                    .color(COLOR_WARNING);

                if let Err(e) = channel_id
                    .send_message(
                        &ctx.http,
                        CreateMessage::new()
                            .content(format!("{}", new_member.user.mention()))
                            .embed(embed),
                    )
                    .await
                {
                    eprintln!(
                        "Failed to send verification instructions for {}: {}",
                        new_member.user.name, e
                    );
                }
            }
            Err(e) => {
                eprintln!("Error verifying new member {}: {}", new_member.user.name, e);
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), String> {
    let config_str = tokio::fs::read_to_string("config.toml")
        .await
        .map_err(|err| format!("Error when accessing config: {err}"))?;

    let config = toml::from_str::<Config>(&config_str)
        .map_err(|err| format!("Error when deserializing config: {err}"))?;

    let http_client = reqwest::Client::new();

    let intents =
        serenity::GatewayIntents::non_privileged() | serenity::GatewayIntents::GUILD_MEMBERS;

    let config_clone = config.clone();
    let http_client_clone = http_client.clone();
    let guild_id = serenity::GuildId::new(config.guild_id);

    let owners = std::collections::HashSet::from([serenity::UserId::new(config.owner_id)]);

    let framework = poise::Framework::<BotData, Error>::builder()
        .options(poise::FrameworkOptions {
            commands: vec![verify(), verify_for(), whois(), player(), verify_all()],
            owners,
            ..Default::default()
        })
        .setup(move |ctx, ready, framework| {
            Box::pin(async move {
                println!("Bot logged in as {}", ready.user.name);
                println!(
                    "Registering {} commands in guild {}",
                    framework.options().commands.len(),
                    guild_id
                );
                poise::builtins::register_in_guild(ctx, &framework.options().commands, guild_id)
                    .await?;
                println!("Commands registered successfully");
                Ok(BotData {
                    config: config_clone,
                    http_client: http_client_clone,
                })
            })
        })
        .build();

    let client = serenity::ClientBuilder::new(&config.discord_token, intents)
        .framework(framework)
        .event_handler(NewUserHandler {
            config: config.clone(),
            http_client,
        })
        .await;
    client.unwrap().start().await.unwrap();

    Ok(())
}
