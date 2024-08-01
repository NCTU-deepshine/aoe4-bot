create table if not exists accounts (
  id serial primary key,
  user_id bigint not null,
  aoe4_id bigint not null unique
);

create table if not exists reminders (
    user_id bigint primary key,
    days integer not null,
    last_played timestamp with time zone,
    last_reminded timestamp with time zone
)