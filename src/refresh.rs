use crate::db::{list_all, list_reminder_needed, reminder_update_last_reminded, to_user_id};
use crate::ranked::{RankedPlayer, try_create_ranked_from_account};
use crate::{Data, Error};
use chrono::Utc;
use poise::futures_util::StreamExt;
use poise::futures_util::stream;
use serenity::all::{ChannelId, CreateMessage, Http};
use std::collections::HashMap;
use tracing::{error, info};

static RANK_CHANNEL_ID: ChannelId = ChannelId::new(1263079883937153105);

pub(crate) async fn do_refresh(http: &Http, data: &Data) -> Result<(), Error> {
    info!("attempting to refresh");

    let accounts = list_all(&data.database).await.inspect_err(|_error| {
        error!("database query failed");
    })?;
    let players = stream::iter(accounts)
        .filter_map(|account| try_create_ranked_from_account(http, data, account))
        .collect::<Vec<RankedPlayer>>()
        .await;
    let mut unique_players = players
        .into_iter()
        .fold(HashMap::new(), |mut acc, player| {
            acc.entry(String::from(player.discord_username()))
                .or_insert_with(Vec::new)
                .push(player);
            acc
        })
        .into_values()
        .filter_map(|mut list| {
            list.sort();
            let sorted = list;
            sorted.into_iter().reduce(|mut acc, player| {
                acc.append_alt(player);
                acc
            })
        })
        .collect::<Vec<RankedPlayer>>();
    info!("finish ranked player collection");

    unique_players.sort();
    let sorted_players = unique_players;
    info!("collected and sorted {} players", sorted_players.len());

    info!("clearing all existing messages in the channel");
    let messages = http
        .get_messages(RANK_CHANNEL_ID, None, None)
        .await
        .inspect_err(|_error| {
            error!("getting message from discord channel failed");
        })?;

    for message_id in messages.iter().map(|message| message.id) {
        http.delete_message(RANK_CHANNEL_ID, message_id, None)
            .await
            .inspect_err(|_error| {
                error!("deleting existing messages from discord failed");
            })?;
    }

    let mut buffer = String::new();
    for (i, player) in sorted_players.iter().enumerate() {
        let text = format!("第{}名  {}\n_ _\n", i + 1, player);
        buffer = buffer + &text;

        if i % 5 == 4 {
            send_rankings(http, &buffer).await?;
            buffer = String::new();
        }
    }

    if !buffer.is_empty() {
        send_rankings(http, &buffer).await?;
    }

    Ok(())
}
async fn send_rankings(http: &Http, content: &String) -> Result<(), Error> {
    http.get_channel(RANK_CHANNEL_ID)
        .await?
        .guild()
        .unwrap()
        .say(http, content)
        .await?;
    Ok(())
}

pub(crate) async fn send_reminders(http: &Http, data: &Data) -> Result<(), Error> {
    info!("starting to send reminders");
    let reminders = list_reminder_needed(&data.database).await;
    for reminder in reminders.iter() {
        let user = http.get_user(to_user_id(reminder.user_id)).await?;
        let days = Utc::now().signed_duration_since(reminder.last_played).num_days();
        if user
            .direct_message(
                &http,
                CreateMessage::new().content(format!("溫馨提醒：已經耍廢{}天囉 該爬天梯了！", days)),
            )
            .await
            .is_ok()
        {
            reminder_update_last_reminded(&data.database, reminder.user_id).await
        }
    }

    Ok(())
}
