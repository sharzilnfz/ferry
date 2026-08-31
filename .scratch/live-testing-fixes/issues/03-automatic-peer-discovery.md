# 03: Automatic local network mDNS and mesh peer discovery in daemon supervisor

**What to build:** Background sync daemons automatically discover other instances on the local WiFi network via mDNS and over mesh networks via peer directory routing. When two paired devices with mutual authorizations for a folder run Ferry, the daemon supervisor automatically initiates and maintains bidirectional sync connections without requiring the user to pass manual listen ports, IP addresses, or peer URLs.

**Blocked by:** 02: Network rendezvous over P2P topic and mutual key wrapping for share and join

**Status:** ready-for-agent

- [ ] Local network mDNS service discovery and advertisement are enabled by default on sync transport endpoints
- [ ] Daemon supervisor inspects authorized peer keys from the folder configuration for all registered folders
- [ ] Discovered peer network addresses are automatically mapped in the transport route table
- [ ] Daemons automatically dial and establish sync sessions with authorized peers without manual peer URL flags
- [ ] Automated multi-daemon tests verify that two instances on the same network discover each other and converge file changes autonomously
