# Webhook Forwarder

A very simple webhook forwarder. A rust based alternative to https://github.com/probot/smee-client and https://github.com/probot/smee.io. (Does not conform to the same protocol). Currently there is no public hosting :( but there is a `Containerfile` for hosting your own.

Clients can either use the CLI tool or the client as rust library using cargo.

Install the CLI with

```bash
cargo install --git https://github.com/RuairidhWilliamson/webhook-forwarder webhook-forwarder
```

or add the client library to your project with

```bash
cargo add --git https://github.com/RuairidhWilliamson/webhook-forwarder webhook-forwarder
```
