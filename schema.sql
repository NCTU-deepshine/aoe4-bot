create table if not exists accounts (
  id serial primary key,
  user_id bigint not null,
  aoe4_id bigint not null unique,
);
