alter table tournaments add column registration_mode text not null default 'open'
  check (registration_mode in ('open','invite_only'));
