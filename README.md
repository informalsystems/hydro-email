# Hydro Email 

- [Hydro Email Kanban](https://github.com/orgs/Lay3rLabs/projects/14/views/1): project board.
- [Tooling](docs/Tooling.md): setup your development environment.
- [GettingStarted](docs/GettingStarted.md): get started developing and testing the code.
- [AgentMail](docs/AgentMail.md): set up the AgentMail inbox this reads mail from.
- [LocalEmail](docs/LocalEmail.md): run local email server for testing. 
- [Codebase](docs/Codebase.md): overview of the code structure

# Notes

* This uses DKIM verification from our fork of Cloudflare's DKIM library: https://github.com/Lay3rLabs/dkim-wasi
* `wasi:tls` is not on wa.dev, so we just clone our wit repo, but `wkg` is still used to fetch dependencies
