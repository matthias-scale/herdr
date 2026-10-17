620 standards lane (native Opus medium, requested gpt-6-luna): 0 blocking, 4 P3
- P3 src/ui/sidebar.rs:4124 unreachable branch now hardcodes project "herdr"
- P3 src/ui/mobile.rs:1139 literal "projects" instead of fleet_grouping::PROJECTS
- P3 src/ui/sidebar/fleet_grouping.rs:118 bare numeric status indices (3 = working) in 5 places
- P3 src/ui/sidebar.rs:13075 UI evidence is TestBackend only; live-build capture pending (covered by Matthias QA in tab QA #620)
