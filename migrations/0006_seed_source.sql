alter table tournaments add column seed_source text not null default 'suggested'
  check (seed_source in ('suggested','manual'));
