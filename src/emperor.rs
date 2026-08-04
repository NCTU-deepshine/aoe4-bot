use crate::guilds::{Feature, Guilds};
use rand::Rng;
use serenity::all::{EmojiId, Message, Reaction, ReactionType, Ready, UserId};
use serenity::async_trait;
use serenity::prelude::*;
use tracing::info;

pub(crate) struct Emperor {
    guilds: Guilds,
}

impl Emperor {
    pub(crate) fn new(guilds: Guilds) -> Self {
        Self { guilds }
    }

    fn select_emoji() -> ReactionType {
        let num = rand::rng().random_range(0..100);
        if num == 0 {
            ReactionType::from('🍄')
        } else if num <= 10 {
            ReactionType::from('🐷')
        } else {
            ReactionType::from(EmojiId::new(1299285258457448522))
        }
    }
}

#[async_trait]
impl EventHandler for Emperor {
    async fn message(&self, ctx: Context, new_message: Message) {
        // Home guild only. This handler matches on user ids and keywords with no
        // notion of where it is, so without the guard it would start reacting in the
        // tournament guild the moment the bot joins (docs/tournament.md §8.0).
        if !self.guilds.allows(Feature::Home, new_message.guild_id) {
            return;
        }

        let emperor = UserId::new(453010726311821322);
        let emperor2 = UserId::new(1511740443132428328);
        let knockgod = UserId::new(364796522396647424);
        let baltune = UserId::new(202510973519527937);
        let racoon = UserId::new(302663000463114242);
        let author = new_message.author.id;
        let content = &new_message.content;
        let mut blocked = false;
        if author == emperor
            || author == emperor2
            || content.contains("天子")
            || content.contains("唱歌")
            || new_message.mentions_user_id(emperor)
            || new_message.mentions_user_id(emperor2)
        {
            blocked = Self::detect_blocked(new_message.react(&ctx.http, Emperor::select_emoji()).await);
        }
        if content.contains("那可")
            || content.contains("納可")
            || content.contains("knock")
            || new_message.mentions_user_id(knockgod)
        {
            blocked = Self::detect_blocked(
                new_message
                    .react(&ctx.http, ReactionType::from(EmojiId::new(1264746593366839431)))
                    .await,
            );
        }
        if content.contains("平等院") || content.contains("海門城堡") {
            blocked = Self::detect_blocked(
                new_message
                    .react(&ctx.http, ReactionType::from(EmojiId::new(1338936646615306250)))
                    .await,
            );
        }
        if content.contains("balt")
            || content.contains("Balt")
            || content.contains("包吞")
            || new_message.mentions_user_id(baltune)
        {
            blocked = Self::detect_blocked(
                new_message
                    .react(&ctx.http, ReactionType::from(EmojiId::new(1264326708962525225)))
                    .await,
            );
        }
        if content.contains("城主")
            || content.contains("成主")
            || (content.contains("all") && content.contains("in"))
            || content.contains("快攻")
            || content.contains("試煉")
            || content.contains("喝水")
            || content.contains("諸葛弩")
            || content.contains("議會廳")
            || content.contains("競技場")
            || content.contains("勝利塔")
            || content.contains("衝車")
            || content.contains("搓車")
        {
            blocked = Self::detect_blocked(new_message.react(&ctx.http, ReactionType::from('🦧')).await);
        }
        if content.contains("象") {
            blocked = Self::detect_blocked(new_message.react(&ctx.http, ReactionType::from('🐘')).await);
        }
        if author == racoon {
            blocked = Self::detect_blocked(new_message.react(&ctx.http, ReactionType::from('🦝')).await);
        }

        if blocked {
            let num = rand::rng().random_range(0..10);
            if num == 0 {
                let channel = ctx
                    .http
                    .get_channel(new_message.channel_id)
                    .await
                    .unwrap()
                    .guild()
                    .unwrap();
                channel.say(ctx.http, "<:emoji_93:1299285258457448522>").await.unwrap();
            }
        }
    }

    async fn ready(&self, _: Context, ready: Ready) {
        info!("{} emperor bot is connected!", ready.user.name);
    }
}

impl Emperor {
    fn detect_blocked(result: serenity::Result<Reaction>) -> bool {
        match result {
            Ok(_) => false,
            Err(error) => {
                if let serenity::Error::Http(HttpError::UnsuccessfulRequest(error_response)) = error
                    && error_response.error.message == "Reaction blocked"
                {
                    // handle blocked reaction
                    return true;
                }
                false
            },
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_contains() {
        assert!(String::from("比那明居天子").contains("天子"))
    }
}
