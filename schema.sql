-- The reminder feature was removed; drop its table where it still exists.
drop table if exists reminders;

create table if not exists accounts (
  id integer primary key autoincrement,
  user_id bigint not null,
  aoe4_id bigint not null unique
);
