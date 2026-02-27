--
-- PostgreSQL database dump
--

\restrict ahfQ1HI3wd2mLeKLSUAqPXuLSrfFic7abHSRqnnr66033t44RtWPS2DsPgCK629

-- Dumped from database version 17.4 - Percona Server for PostgreSQL 17.4.1
-- Dumped by pg_dump version 17.7 (Debian 17.7-3.pgdg13+1)

SET statement_timeout = 0;
SET lock_timeout = 0;
SET idle_in_transaction_session_timeout = 0;
SET transaction_timeout = 0;
SET client_encoding = 'UTF8';
SET standard_conforming_strings = on;
SELECT pg_catalog.set_config('search_path', '', false);
SET check_function_bodies = false;
SET xmloption = content;
SET client_min_messages = warning;
SET row_security = off;

--
-- Name: pgbouncer; Type: SCHEMA; Schema: -; Owner: postgres
--

CREATE SCHEMA pgbouncer;


ALTER SCHEMA pgbouncer OWNER TO postgres;

--
-- Name: pg_stat_monitor; Type: EXTENSION; Schema: -; Owner: -
--

CREATE EXTENSION IF NOT EXISTS pg_stat_monitor WITH SCHEMA public;


--
-- Name: EXTENSION pg_stat_monitor; Type: COMMENT; Schema: -; Owner: 
--

COMMENT ON EXTENSION pg_stat_monitor IS 'The pg_stat_monitor is a PostgreSQL Query Performance Monitoring tool, based on PostgreSQL contrib module pg_stat_statements. pg_stat_monitor provides aggregated statistics, client information, plan details including plan, and histogram information.';


--
-- Name: pgaudit; Type: EXTENSION; Schema: -; Owner: -
--

CREATE EXTENSION IF NOT EXISTS pgaudit WITH SCHEMA public;


--
-- Name: EXTENSION pgaudit; Type: COMMENT; Schema: -; Owner: 
--

COMMENT ON EXTENSION pgaudit IS 'provides auditing functionality';


--
-- Name: get_auth(text); Type: FUNCTION; Schema: pgbouncer; Owner: postgres
--

CREATE FUNCTION pgbouncer.get_auth(username text) RETURNS TABLE(username text, password text)
    LANGUAGE sql STABLE SECURITY DEFINER
    AS $_$
  SELECT rolname::TEXT, rolpassword::TEXT
  FROM pg_catalog.pg_authid
  WHERE pg_authid.rolname = $1
    AND pg_authid.rolcanlogin
    AND NOT pg_authid.rolsuper
    AND NOT pg_authid.rolreplication
    AND pg_authid.rolname <> '_crunchypgbouncer'
    AND (pg_authid.rolvaliduntil IS NULL OR pg_authid.rolvaliduntil >= CURRENT_TIMESTAMP)$_$;


ALTER FUNCTION pgbouncer.get_auth(username text) OWNER TO postgres;

SET default_tablespace = '';

SET default_table_access_method = heap;

--
-- Name: accounts; Type: TABLE; Schema: public; Owner: schema_admin
--

CREATE TABLE accounts (
    id integer NOT NULL,
    user_id bigint NOT NULL,
    aoe4_id bigint NOT NULL
);


ALTER TABLE accounts OWNER TO schema_admin;

--
-- Name: accounts_id_seq; Type: SEQUENCE; Schema: public; Owner: schema_admin
--

CREATE SEQUENCE accounts_id_seq
    AS integer
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


ALTER SEQUENCE accounts_id_seq OWNER TO schema_admin;

--
-- Name: accounts_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: schema_admin
--

ALTER SEQUENCE accounts_id_seq OWNED BY public.accounts.id;


--
-- Name: reminders; Type: TABLE; Schema: public; Owner: schema_admin
--

CREATE TABLE reminders (
    user_id bigint NOT NULL,
    days integer NOT NULL,
    last_played timestamp with time zone,
    last_reminded timestamp with time zone
);


ALTER TABLE reminders OWNER TO schema_admin;

--
-- Name: accounts id; Type: DEFAULT; Schema: public; Owner: schema_admin
--

ALTER TABLE ONLY accounts ALTER COLUMN id SET DEFAULT nextval('public.accounts_id_seq'::regclass);


--
-- Data for Name: accounts; Type: TABLE DATA; Schema: public; Owner: schema_admin
--

COPY accounts (id, user_id, aoe4_id) FROM stdin;
1	182108123174010880	199837
2	408258483339657217	3829877
3	569452196895260695	812594
4	687726735911026869	4546076
5	472412938804789249	9090293
6	1447229191748587592	11992441
7	979062427054383205	18262439
8	336441392421142528	10297023
9	344073102952497163	10870965
10	849670025441837106	5771402
11	820100226831351811	9242849
12	820100226831351811	930687933
13	849670025441837106	1212274608
14	323153740603457537	23330953
15	455381082649395226	19611086
16	455381082649395226	18338664
17	720955323183267840	2979974
18	609779193319784674	10929811
19	302663000463114242	20105616
20	569522497502838784	16262157
21	614100956342124566	10855038
22	357494498600943616	219632
23	769805958179192832	1154902396
24	436398373839568896	19815828
25	455381082649395226	22166647
26	1172376833606570025	20467959
27	1328441763547054161	11005888
28	1328441763547054161	7656
29	711519131769241620	11030503
30	414307565565116427	782384
31	401333995574525954	21754648
32	537579750676234261	8479687
33	919520645312827413	10894661
34	569522497502838784	7449735
35	309851312881926144	19068445
36	422957281060192277	10857653
37	295687390859755531	21606746
38	449817367778689025	4539664
39	382431785977380865	7639078
40	309851312881926144	20986071
41	474810121499967488	4981020
42	474810121499967488	313293502
43	1033001614241431642	6952680
44	352344177733926912	9209204
45	280324842471817216	4145882
46	931814423901921310	6971921
47	323153740603457537	20205355
48	1238159840740905030	15371215
49	452469145271926797	6523183
50	654300891180171283	11469613
51	818505906517049375	405236
52	323153740603457537	368062
53	202510973519527937	4583101
54	931814423901921310	17337623
55	549615087065759764	8658496
56	997118859855274077	15119139
57	302695598539145216	20337191
58	364072676592975873	6160299
59	720955323183267840	20565513
60	202225245300457472	20252603
61	230335883985944576	10895154
62	407854525916119040	11465
63	380688858729414658	20563137
64	419422206880251904	920420
65	470295845460508677	6786474
66	1205153752315596803	1277190
67	235908166964084737	11670625
68	235908166964084737	19465074
69	803513960300544000	6965777
70	531898747022606336	1535563
71	697082566926401596	18754680
72	235908166964084737	11436759
73	402475874299150336	3570429
74	230666276832411648	15354773
75	847492638155866112	18242141
76	144505580214550538	6536995
77	517306023342768129	11628131
78	302695598539145216	7008236
157	380688858729414658	3763401
158	230666276832411648	664843
159	380688858729414658	1
160	578992658823905284	10881495
161	604687710355193866	10894296
162	235908166964084737	11323971
163	720955323183267840	13753974
164	454058102824763403	1037911
165	235908166964084737	5075657
166	364796522396647424	570060
169	345916428475695104	7670684
170	537969842544967681	23011282
171	1238159840740905030	20524200
172	546225849104334849	10914023
174	872687335286898768	22656843
\.


--
-- Data for Name: reminders; Type: TABLE DATA; Schema: public; Owner: schema_admin
--

COPY reminders (user_id, days, last_played, last_reminded) FROM stdin;
537969842544967681	5	2026-02-27 09:46:11+00	2026-02-19 00:01:45.911862+00
\.


--
-- Name: accounts_id_seq; Type: SEQUENCE SET; Schema: public; Owner: schema_admin
--

SELECT pg_catalog.setval('accounts_id_seq', 175, true);


--
-- Name: accounts accounts_aoe4_id_key; Type: CONSTRAINT; Schema: public; Owner: schema_admin
--

ALTER TABLE ONLY accounts
    ADD CONSTRAINT accounts_aoe4_id_key UNIQUE (aoe4_id);


--
-- Name: accounts accounts_pkey; Type: CONSTRAINT; Schema: public; Owner: schema_admin
--

ALTER TABLE ONLY accounts
    ADD CONSTRAINT accounts_pkey PRIMARY KEY (id);


--
-- Name: reminders reminders_pkey; Type: CONSTRAINT; Schema: public; Owner: schema_admin
--

ALTER TABLE ONLY reminders
    ADD CONSTRAINT reminders_pkey PRIMARY KEY (user_id);


--
-- Name: SCHEMA pgbouncer; Type: ACL; Schema: -; Owner: postgres
--

GRANT USAGE ON SCHEMA pgbouncer TO _crunchypgbouncer;


--
-- Name: SCHEMA public; Type: ACL; Schema: -; Owner: pg_database_owner
--

GRANT ALL ON SCHEMA public TO schema_admin;


--
-- Name: FUNCTION get_auth(username text); Type: ACL; Schema: pgbouncer; Owner: postgres
--

REVOKE ALL ON FUNCTION pgbouncer.get_auth(username text) FROM PUBLIC;
GRANT ALL ON FUNCTION pgbouncer.get_auth(username text) TO _crunchypgbouncer;


--
-- Name: DEFAULT PRIVILEGES FOR SEQUENCES; Type: DEFAULT ACL; Schema: public; Owner: fly-user
--

ALTER DEFAULT PRIVILEGES FOR ROLE "fly-user" IN SCHEMA public GRANT ALL ON SEQUENCES TO schema_admin;


--
-- Name: DEFAULT PRIVILEGES FOR TABLES; Type: DEFAULT ACL; Schema: public; Owner: fly-user
--

ALTER DEFAULT PRIVILEGES FOR ROLE "fly-user" IN SCHEMA public GRANT ALL ON TABLES TO schema_admin;


--
-- PostgreSQL database dump complete
--

\unrestrict ahfQ1HI3wd2mLeKLSUAqPXuLSrfFic7abHSRqnnr66033t44RtWPS2DsPgCK629

