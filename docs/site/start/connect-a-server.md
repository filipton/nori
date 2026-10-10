---
title: Connect a server
description: Signing in to Navidrome, Subsonic, OpenSubsonic or octo-fiesta, with the advanced options.
sidebar:
  order: 3
---

Applies to nori 0.6. Every option is on Android; the desktop, terminal and iPod note what they lack.

Open nori and enter the **Server URL**, your **User** and **Password**, then **Connect**. nori talks to
Navidrome, octo-fiesta and any Subsonic server. If you leave out `https://`, nori adds it.

## Authentication

- **Token** authentication is used by default: the password itself is never sent.
- **API key instead of password (OpenSubsonic)**: paste a key your server made for you.
- **Legacy authentication**: for old servers without token auth. nori switches to it by itself when the
  server says it needs it.

## Advanced

Open **Advanced** on the sign-in screen for:

| Option | What it does |
|---|---|
| Name | what the server is called in the server list |
| Second address | used when the first one does not answer, for example away from home; a bitrate limit can apply to it |
| Extra HTTP headers | one per line, `Name: value`, for reverse proxies, basic auth or Cloudflare Access |
| Accept self-signed certificate | only for your own server; certificates you installed in Android are accepted without it |
| Client certificate | import a `.p12` file with its password, for servers that ask for mutual TLS |
| Wi-Fi only | never contact this server over mobile data |

You can keep several servers and switch between them in Settings. Each keeps its own library index.

:::note[Desktop, terminal and iPod]
The desktop, terminal and iPod clients sign in with an address, user and password or an API key, and a
second address. They do not apply extra headers or accept self-signed certificates yet.
:::

## octo-fiesta

octo-fiesta sits in front of your Subsonic server. nori recognises its provider songs, marks them, and
never queues or downloads one you did not ask for, since playing one makes the server download it. It is
also what [remote control](../connected/remote-control.md) away from home and
[jams](../connected/jams.md) go through.
