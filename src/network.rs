use ed25519_dalek::{pkcs8::DecodePrivateKey, SigningKey};
use futures::prelude::*;
use hex_literal::hex;
use libp2p::gossipsub::{Behaviour, PublishError};
use libp2p::kad::QueryResult;
use libp2p::swarm::ConnectionError;
use libp2p::{
    gossipsub::{self, IdentTopic, MessageAuthenticity, MessageId, ValidationMode},
    identify,
    identity::{Keypair, PublicKey},
    kad::{self, store::MemoryStore},
    multiaddr,
    swarm::{self, NetworkBehaviour, SwarmEvent},
    tcp, yamux, Multiaddr, PeerId, Swarm,
};
use libp2p_mdns as mdns;
use libp2p_tls as tls;
use log::{debug, error, info, warn};
use machine_uid;
use std::collections::{hash_map::DefaultHasher, HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::{
    error::Error,
    net::{Ipv4Addr, SocketAddrV4},
    time::{Duration, SystemTime},
};
use tokio::sync::mpsc;

#[derive(NetworkBehaviour)]
struct P2pClipboardBehaviour {
    gossipsub: Behaviour<CompressionTransform>,
    kademlia: kad::Behaviour<MemoryStore>,
    identify: identify::Behaviour,
    mdns: mdns::tokio::Behaviour,
}

#[derive(Clone, Eq, Hash, PartialEq)]
struct PeerEndpointCache {
    peer_id: PeerId,
    address: Multiaddr,
}

#[derive(Default, Clone)]
struct CompressionTransform;

// Not used directly, we will derive new keys using machine ID from this key.
// We have to do this to have a stable Peer ID when no key is specified by the user.
const ID_SEED: [u8; 118] = hex!("2d2d2d2d2d424547494e2050524956415445204b45592d2d2d2d2d0a4d43344341514177425159444b3256774243494549444c3968565958485271304f48386f774a72363169416a45385a52614263363254373761723564397339670a2d2d2d2d2d454e442050524956415445204b45592d2d2d2d2d");

pub async fn start_network(
    rx: mpsc::Receiver<String>,
    tx: mpsc::Sender<String>,
    connect_arg: Option<Vec<String>>,
    key_arg: Option<String>,
    listen_arg: Option<String>,
    psk: Option<String>,
    disable_mdns: bool,
) -> Result<(), Box<dyn Error>> {
    let id_keys = match key_arg {
        Some(arg) => {
            let pem = std::fs::read_to_string(arg)?;
            let mut verifying_key_bytes = SigningKey::from_pkcs8_pem(&pem)?.to_bytes();
            Keypair::ed25519_from_bytes(&mut verifying_key_bytes)?
        }
        None => {
            let id: String = machine_uid::get()?;
            let mut key_bytes =
                SigningKey::from_pkcs8_pem(std::str::from_utf8(&ID_SEED)?)?.to_bytes();
            let key = Keypair::ed25519_from_bytes(&mut key_bytes)?;
            let mut new_key = key
                .derive_secret(id.as_ref())
                .expect("can derive secret for ed25519");
            Keypair::ed25519_from_bytes(&mut new_key)?
        }
    };
    let peer_id = PeerId::from(id_keys.public());
    info!("Local peer id: {}", peer_id.to_base58());

    // Create a Gossipsub topic
    let gossipsub_topic = IdentTopic::new("p2p_clipboard");

    // get optional boot address and peerId
    let (boot_addr, boot_peer_id) = match connect_arg {
        Some(arg) => {
            // Clap should already guarantee length == 2, just for sanity
            if arg.len() == 2 {
                let peer_id = arg[1].clone().parse::<PeerId>();
                let addr_input = arg[0].clone();
                let sock_addr = parse_ipv4_with_port(Some(addr_input));
                let multiaddr = match sock_addr {
                    Ok((ip, port)) => Ok(format!("/ip4/{}/tcp/{}", ip, port)
                        .parse::<Multiaddr>()
                        .unwrap()),
                    Err(_) => Err(()),
                }
                .unwrap_or_else(|_| {
                    error!("Connect address is not a valid socket address");
                    std::process::exit(1);
                });
                (Some(multiaddr), peer_id.ok())
            } else {
                (None, None)
            }
        }
        None => (None, None),
    };

    // Create a Swarm to manage peers and events.
    let mut swarm: Swarm<P2pClipboardBehaviour> = {
        let mut chat_behaviour = P2pClipboardBehaviour {
            gossipsub: create_gossipsub_behavior(id_keys.clone()),
            kademlia: create_kademlia_behavior(peer_id),
            identify: create_identify_behavior(id_keys.public()),
            mdns: create_mdns_behavior(peer_id, psk.clone(), disable_mdns),
        };

        // subscribes to our topic
        chat_behaviour
            .gossipsub
            .subscribe(&gossipsub_topic)
            .unwrap();

        libp2p::SwarmBuilder::with_existing_identity(id_keys)
            .with_tokio()
            .with_tcp(
                tcp::Config::default(),
                tls::Config::new_with_psk(psk),
                yamux::Config::default,
            )?
            .with_behaviour(|_key| chat_behaviour)?
            .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
            .build()
    };

    swarm
        .behaviour_mut()
        .kademlia
        .set_mode(Some(kad::Mode::Server));

    let multiaddr = match listen_arg {
        Some(socket_addr_string) => {
            if let Ok((ip, port)) = parse_ipv4_with_port(Some(socket_addr_string)) {
                Ok(format!("/ip4/{}/tcp/{}", ip, port))
            } else {
                Err(())
            }
        }
        // Listen on all interfaces and whatever port the OS assigns
        None => Ok("/ip4/0.0.0.0/tcp/0".to_string()),
    }
    .unwrap_or_else(|_| {
        error!("Listen address is not a valid socket address");
        std::process::exit(1);
    });

    let _ = swarm.listen_on(multiaddr.parse()?).unwrap_or_else(|_| {
        error!("Cannot listen on specified address");
        std::process::exit(1);
    });

    // FIXME: Can we swap the boot_node to some fallback node if it is temporarily unavailable?
    let boot_node = {
        // Reach out to another node if specified
        if let Some(boot_addr) = boot_addr {
            debug!("Will dial {}", &boot_addr);
            swarm
                .behaviour_mut()
                .kademlia
                .add_address(&boot_peer_id.unwrap(), boot_addr.clone());
            let _ = swarm.dial(boot_addr.clone());
            let _ = swarm.disconnect_peer_id(boot_peer_id.unwrap());
            Some(PeerEndpointCache {
                peer_id: boot_peer_id.unwrap(),
                address: boot_addr.clone(),
            })
        } else {
            None
        }
    };

    let swarm_handle = tokio::spawn(run(swarm, gossipsub_topic, rx, tx, boot_node));
    swarm_handle.await?;
    Ok(())
}

async fn run(
    mut swarm: Swarm<P2pClipboardBehaviour>,
    gossipsub_topic: IdentTopic,
    mut rx: mpsc::Receiver<String>,
    tx: mpsc::Sender<String>,
    boot_node: Option<PeerEndpointCache>,
) {
    // We have to cache all endpoints so that we can reconnect to the p2p networks when our IP has changed.
    let mut endpoint_cache: VecDeque<PeerEndpointCache> = VecDeque::new();
    let mut unique_endpoints: HashSet<PeerEndpointCache> = HashSet::new();
    let mut current_listen_addresses: HashSet<Multiaddr> = HashSet::new();
    // We also need to cache the announced identity for each node, because they will be different from what we actually connects.
    let mut announced_identities: HashMap<PeerId, Vec<Multiaddr>> = HashMap::new();
    let mut t = SystemTime::now();
    let mut sleep;
    loop {
        let to_publish = {
            sleep = Box::pin(tokio::time::sleep(Duration::from_secs(30)).fuse());
            tokio::select! {
                Some(message) = rx.recv() => {
                    debug!("Received local clipboard: {}", message.clone());
                    Some((gossipsub_topic.clone(), message.clone()))
                }
                event = swarm.select_next_some() => match event {
                    SwarmEvent::Behaviour(P2pClipboardBehaviourEvent::Gossipsub(ref gossip_event)) => {
                        if let gossipsub::Event::Message {
                            propagation_source: peer_id,
                            message_id: id,
                            message,
                        } = gossip_event
                        {
                            debug!("Got message: {} with id: {} from peer: {:?}",
                            String::from_utf8_lossy(&message.data),
                            id,
                            peer_id);
                            tx.send(String::from_utf8_lossy(&message.data).parse().unwrap()).await.expect("Panic when sending to channel");
                        }
                        None
                    }
                    SwarmEvent::Behaviour(P2pClipboardBehaviourEvent::Identify(ref identify_event)) => {
                        match identify_event {
                            identify::Event::Received {
                                peer_id,
                                info:
                                identify::Info {
                                    listen_addrs,
                                    ..
                                },
                            } => {
                                // We will receive identify info for 3 reasons:
                                // 1. A new peer want to give us its info for negotiation
                                // 2. An existing peer periodically ping us with these info to show existence
                                // 3. An existing peer has its network config changed and want to tell us its new address
                                // FIXME: In some cases the peers could behind some kind of NAT so they can have their addresses changed without announcing the change.
                                let old_addrs = announced_identities.insert(
                                    *peer_id,
                                    listen_addrs.clone()
                                );
                                if let Some(old_vec) = old_addrs {
                                    let new: HashSet<Multiaddr> = listen_addrs.iter().cloned().collect();
                                    let old: HashSet<Multiaddr> = old_vec.iter().cloned().collect();
                                    // Announced in old but not in new announcement, remove it from routing table
                                    let changes = old.difference(&new);
                                    for addr in changes {
                                        debug!("Removing expired addr {addr} trough identify");
                                        swarm.behaviour_mut().kademlia.remove_address(&peer_id, addr);
                                    }
                                }
                                for addr in listen_addrs {
                                    debug!("received addr {addr} trough identify");
                                    if addr.iter().collect::<Vec<_>>()[0] != multiaddr::Protocol::Ip4(Ipv4Addr::new(127, 0, 0, 1)) {
                                        swarm.behaviour_mut().kademlia.add_address(&peer_id, addr.clone());
                                    }
                                }
                            }
                            _ => {
                                debug!("got other identify event");
                            }
                        }
                        None
                    }
                    SwarmEvent::Behaviour(P2pClipboardBehaviourEvent::Kademlia(ref kad_event)) => {
                        match kad_event {
                            kad::Event::RoutingUpdated {peer, ..} => {
                                debug!("Routing updated for {:#?}", peer);
                            },
                            kad::Event::OutboundQueryProgressed {
                                result: QueryResult::GetClosestPeers(result),
                                ..
                            } => {
                            match result {
                                Ok(kad::GetClosestPeersOk { key: _, peers }) => {
                                    if !peers.is_empty() {
                                        debug!("Query finished with closest peers: {:#?}", peers);
                                        for peer in peers {
                                            debug!("Got peer {peer}");
                                        }
                                    } else {
                                        error!("Query finished with no closest peers.")
                                    }
                                }
                                Err(kad::GetClosestPeersError::Timeout { peers, .. }) => {
                                    if !peers.is_empty() {
                                        error!("Query timed out with closest peers: {:#?}", peers);
                                        for peer in peers {
                                            debug!("Got peer {peer}");
                                        }
                                    } else {
                                        error!("Query timed out with no closest peers.");
                                    }
                                }
                            };
                        }
                            _ => {}
                        }
                        None
                    }
                    SwarmEvent::NewListenAddr { ref address, .. } => {
                        info!("Local node is listening on {address}");
                        let non_local_addr_count = current_listen_addresses
                            .iter()
                            .filter(|&addr| addr.iter().collect::<Vec<_>>()[0]!=multiaddr::Protocol::Ip4(Ipv4Addr::new(127, 0, 0, 1)))
                            .count();
                        current_listen_addresses.insert(address.clone());
                        let mut peers_to_push: Vec<PeerId> = Vec::new();
                        if let Some(boot_node_clone) = boot_node.as_ref() {
                            peers_to_push.push(boot_node_clone.peer_id.clone());
                        }
                        swarm.behaviour_mut().identify.push(peers_to_push.clone());
                        let connected_peers = swarm.connected_peers();
                        let connected_peers_count = connected_peers.count();
                        debug!("Connected to {connected_peers_count} peers");
                        if let None = boot_node.as_ref() {
                            info!("No boot node specified. Waiting for connection.");
                        } else if connected_peers_count == 0 || non_local_addr_count == 0 {
                            debug!("No connected peers or recovered from no network, we need to manually re-dial to the boot node");
                            let mut use_fallback = false;
                            if let Some(real_boot_node) = boot_node.as_ref() {
                                let res = swarm.dial(real_boot_node.address.clone());
                                if let Err(_) = res {
                                    use_fallback = true;
                                }
                            } else {
                                use_fallback = true;
                            }
                            if use_fallback {
                                debug!("Boot node not accessible, try all known peers.");
                                for endpoint in endpoint_cache.clone() {
                                    let res = swarm.dial(endpoint.clone().address);
                                    if let Ok(_) = res {
                                        debug!("Dial successful in fallback");
                                        break;
                                    }
                                }
                            }
                        }
                        let _ = swarm.behaviour_mut().kademlia.bootstrap();
                        None
                    }
                    SwarmEvent::ExpiredListenAddr { ref address, .. } => {
                        warn!("Local node no longer listening on {address}");
                        current_listen_addresses.remove(address);
                        let non_local_addr_count = current_listen_addresses
                            .iter()
                            .filter(|&addr| addr.iter().collect::<Vec<_>>()[0]!=multiaddr::Protocol::Ip4(Ipv4Addr::new(127, 0, 0, 1)))
                            .count();
                        let mut peers_to_push: Vec<PeerId> = Vec::new();
                        if let Some(boot_node_clone) = boot_node.as_ref() {
                            peers_to_push.push(boot_node_clone.peer_id.clone());
                        }
                        swarm.behaviour_mut().identify.push(peers_to_push.clone());
                        if non_local_addr_count > 0 {
                            // Because when main address is teared down, the network usually needs some time to recover
                            tokio::time::sleep(Duration::from_secs(1)).await;
                            if let Some(real_boot_node) = boot_node.as_ref() {
                                let _ = swarm.dial(real_boot_node.address.clone());
                            }
                        }
                        None
                    }
                    SwarmEvent::ConnectionEstablished {
                        ref peer_id,
                        ref endpoint,
                        ..
                    } => {
                        let mut d = endpoint.get_remote_address().clone();
                        // We only care about IP and protocol port, ignoring the p2p suffix
                        // However, the endpoint with p2p protocol is the one we actually connected to.
                        if d.iter().count() > 2 {
                            let _ = d.pop();
                            let cache = PeerEndpointCache {
                                peer_id: peer_id.clone(),
                                address: d.clone(),
                            };
                            debug!("Adding endpoint {d} to cache");
                            if !unique_endpoints.insert(cache.clone()) {
                                // The item is already present in the set (duplicate)
                                debug!("endpoint {d} already in cache, reordering");
                                endpoint_cache.retain(|existing_item| existing_item != &cache);
                            }
                            endpoint_cache.push_front(cache);
                            info!("Connected to peer {} with {}", peer_id, d);
                        }
                        None
                    }
                    SwarmEvent::ConnectionClosed { ref peer_id, ref cause,ref endpoint, ref num_established, .. } => {
                        if *num_established == 0 {
                            warn!("Peer {} has disconnected", peer_id);
                            // When the last connection has closed, we should drop that peer from our cache.
                            // Ideally, all entries related with the peer should have already been removed.
                            // However, some edge cases that does not correctly trigger the event handlers do occur in rare cases.
                            // Remove these entries explicitly if that happens.
                            unique_endpoints.retain(|x| x.peer_id != *peer_id);
                            endpoint_cache.retain(|x| x.peer_id != *peer_id);
                        }
                        if let Some(connection_error) = cause.as_ref().clone() {
                            unique_endpoints.retain(|x| x.address != *endpoint.get_remote_address());
                            endpoint_cache.retain(|x| x.address != *endpoint.get_remote_address());
                            match connection_error {
                                ConnectionError::IO(_io_error) => {
                                    // Handle IO error
                                    // An IO error usually means a network problem occurred on local side, and we will try to re-connect.
                                    let mut retry_count = 0;
                                    let max_retries = 3;
                                    let addr = endpoint.get_remote_address();
                                    while endpoint.is_dialer() {
                                        match swarm.dial(addr.clone()) {
                                            Ok(()) => {
                                                // Dial successful, break out of the loop
                                                let _ = swarm.behaviour_mut().kademlia.bootstrap();
                                                debug!("Dial successful!");
                                                break;
                                            }
                                            Err(dial_err) => {
                                                match dial_err {
                                                    swarm::DialError::DialPeerConditionFalse(swarm::dial_opts::PeerCondition::DisconnectedAndNotDialing) => {
                                                        debug!("Stop trying reconnect as we already have a connection going on");
                                                        break;
                                                    }
                                                    _ => {
                                                        error!("Dial Error: {}", dial_err);
                                                        retry_count += 1;
                                                        if retry_count >= max_retries {
                                                            warn!("Max retries reached. Giving up.");
                                                            break;
                                                        }
                                                        let wait_duration = Duration::from_secs(1);
                                                        warn!("Retrying in {} seconds...", wait_duration.as_secs());
                                                        tokio::time::sleep(wait_duration).await;
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        None
                    }
                    SwarmEvent::OutgoingConnectionError {
                        ref peer_id,
                        ref error,
                        connection_id,
                    } => {
                        debug!("OutgoingConnectionError to {peer_id:?} on {connection_id:?} - {error:?}");
                        // we need to decide if this was a critical error and the peer should be removed from the routing table
                        let should_clean_peer = match error {
                            swarm::DialError::Transport(errors) => {
                                // Most of the transport error comes from local end and the remote peer should not be removed.
                                // Even if it is really the remote end, it is hard to tell because if we have multiple
                                // IP addresses and not all of them is able to connect to the remote endpoint, we may still
                                // have other IP address that is able to connect to that peer.
                                debug!("Dial errors len : {:?}", errors.len());
                                let mut non_recoverable = false;
                                for (_addr, err) in errors {
                                    debug!("OutgoingTransport error : {err:?}");

                                    match err {
                                        libp2p::TransportError::MultiaddrNotSupported(addr) => {
                                            error!("Multiaddr not supported : {addr:?}");
                                            // If we can't dial a peer on a given address, we should remove it from the routing table
                                            // Currently we should not have such problem in production as all nodes using selected Multiaddr
                                            // This could occur during development, added for sanity.
                                            non_recoverable = true
                                        }
                                        libp2p::TransportError::Other(err) => {
                                            let should_hold_and_retry = ["NetworkUnreachable"];
                                            if let Some(inner) = err.get_ref() {
                                                let error_msg = format!("{inner:?}");
                                                debug!("Problematic error encountered: {inner:?}");
                                                // This is not the best way to match an Error, but the Error we get here is very complicated, like:
                                                // `Other(Custom { kind: Other, error: Timeout })`
                                                // `Other(Left(Left(Os { code: 51, kind: NetworkUnreachable, message: "Network is unreachable" })`
                                                // `Other(Left(Left(Os { code: 61, kind: ConnectionRefused, message: "Connection refused" })`
                                                // Makes appropriate matching very hard if we want to match a specific type of error.
                                                if should_hold_and_retry.iter().any(|err| error_msg.contains(err)) {
                                                    if let Some(peer) = peer_id {
                                                        warn!("Dial {peer} is scheduled for retry later");
                                                        tokio::time::sleep(Duration::from_secs(1)).await;
                                                        let _ = swarm.dial(peer.clone());
                                                    }
                                                }
                                            };
                                        }
                                    }
                                }
                                non_recoverable
                            }
                            swarm::DialError::NoAddresses => {
                                // We cannot dial peers without addresses
                                error!("OutgoingConnectionError: No address provided");
                                true
                            }
                            swarm::DialError::Aborted => {
                                error!("OutgoingConnectionError: Aborted");
                                false
                            }
                            swarm::DialError::DialPeerConditionFalse(_) => {
                                error!("OutgoingConnectionError: DialPeerConditionFalse");
                                false
                            }
                            swarm::DialError::LocalPeerId { .. } => {
                                error!("OutgoingConnectionError: LocalPeerId: We are dialing ourselves");
                                true
                            }
                            swarm::DialError::WrongPeerId { obtained, endpoint } => {
                                error!("OutgoingConnectionError: WrongPeerId: obtained: {obtained:?}, endpoint: {endpoint:?}");
                                true
                            }
                            swarm::DialError::Denied { cause } => {
                                error!("OutgoingConnectionError: Denied: {cause:?}");
                                true
                            }
                        };

                        if should_clean_peer {
                            if let Some(dead_peer) = peer_id
                            {
                                warn!("Cleaning out dead peer {dead_peer:?}");
                                unique_endpoints.retain(|endpoint| endpoint.peer_id != *dead_peer);
                                endpoint_cache.retain(|endpoint| endpoint.peer_id != *dead_peer);
                                swarm.behaviour_mut().kademlia.remove_peer(dead_peer);
                            }
                        }
                        None
                    }
                    SwarmEvent::Behaviour(P2pClipboardBehaviourEvent::Mdns(mdns::Event::Discovered(list))) => {
                        for (_peer_id, addr) in list {
                            debug!("mDNS discovered a new peer: {_peer_id}");
                            let _ = swarm.dial(addr.clone());
                        }
                        None
                    },
                    SwarmEvent::Behaviour(P2pClipboardBehaviourEvent::Mdns(mdns::Event::Expired(list))) => {
                        for (peer_id, addr) in list {
                            debug!("mDNS expired a peer: {peer_id}");
                            // For most time this address should already be removed.
                            swarm.behaviour_mut().kademlia.remove_address(&peer_id, &addr);
                        }
                        None
                    },
                    _ => {None}
                },
                _ = &mut sleep => {
                    debug!("Long idle detected, doing periodic jobs");
                    let stale_peers: Vec<_> = swarm.behaviour_mut().gossipsub.all_peers()
                        .filter(|(_, topics)| topics.is_empty())
                        .map(|(peer, _)| peer.clone())
                        .collect();
                    for peer in stale_peers {
                        // This is a strange upstream bug. Sometimes a peer may appear connected but without any topic subscriptions.
                        // If this happens we want to drop the connection and wait for the remote peer to reconnect later.
                        let _ = &swarm.disconnect_peer_id(peer);
                    }
                    if let Some(boot) = boot_node.as_ref() {
                        // We started with a boot node, so we want to make sure that we do have at least one peer.
                        // Although the boot node may be offline, we want to connect to it if we don't have any other peers, in case it is started later.
                        let all_peers = swarm.connected_peers();
                        let should_redial_boot_node = all_peers.count() < 1;
                        if should_redial_boot_node {
                            let _ = swarm.dial(boot.address.clone());
                        }
                    }
                    // Look up ourselves to improve awareness in the network.
                    let self_id = *swarm.local_peer_id();
                    swarm.behaviour_mut().kademlia.get_closest_peers(self_id);
                    None
                }
            }
        };
        let d = SystemTime::now()
            .duration_since(t)
            .unwrap_or_else(|_| Duration::from_secs(0));
        t = SystemTime::now();
        if d > Duration::from_secs(60) {
            // We should already update the timer after each loop completes, and we have a 30-second timeout for periodic tasks.
            // If the duration of this loop execution is too long (2 * timeout), our execution may be suspended midway.
            // A common reason for such suspension is the host OS entering a power-saving energy state.
            // We need to break out of the loop and restart swarm since most of our underlying connections will break.
            // The easiest way to recover is to restart.
            warn!("Handler completed longer than expected, restarting swarm");
            return;
        }
        if let Some((topic, line)) = to_publish {
            if let Err(err) = swarm
                .behaviour_mut()
                .gossipsub
                .publish(topic.clone(), line.as_bytes())
            {
                match err {
                    PublishError::Duplicate => {}
                    _ => {
                        error!("Error publishing message: {}", err);
                    }
                }
            }
        }
    }
}

fn create_gossipsub_behavior(id_keys: Keypair) -> Behaviour<CompressionTransform> {
    // Hash the message and use it as ID
    // Duplicated message will be ignored and not sent because they will have same hash
    let message_id_fn = |message: &gossipsub::Message| {
        let mut s = DefaultHasher::new();
        message.data.hash(&mut s);
        MessageId::from(s.finish().to_string())
    };
    impl gossipsub::DataTransform for CompressionTransform {
        fn inbound_transform(
            &self,
            raw_message: gossipsub::RawMessage,
        ) -> Result<gossipsub::Message, std::io::Error> {
            let buf: Vec<u8> = zstd::decode_all(&*raw_message.data)?;
            Ok(gossipsub::Message {
                source: raw_message.source,
                data: buf,
                sequence_number: raw_message.sequence_number,
                topic: raw_message.topic,
            })
        }

        fn outbound_transform(
            &self,
            _topic: &gossipsub::TopicHash,
            data: Vec<u8>,
        ) -> Result<Vec<u8>, std::io::Error> {
            let compressed_bytes = zstd::encode_all(&*data, 0)?;
            debug!("Compressed size {}", compressed_bytes.len());
            Ok(compressed_bytes)
        }
    }

    let gossipsub_config = gossipsub::ConfigBuilder::default()
        .heartbeat_interval(Duration::from_secs(10))
        .validation_mode(ValidationMode::Strict)
        .message_id_fn(message_id_fn)
        .do_px()
        .build()
        .expect("Valid config");
    Behaviour::new_with_transform(
        MessageAuthenticity::Signed(id_keys),
        gossipsub_config,
        None,
        CompressionTransform,
    )
    .expect("Correct configuration")
}

fn create_kademlia_behavior(local_peer_id: PeerId) -> kad::Behaviour<MemoryStore> {
    let mut cfg = kad::Config::default();
    cfg.set_query_timeout(Duration::from_secs(5 * 60));
    let store = MemoryStore::new(local_peer_id);
    kad::Behaviour::with_config(local_peer_id, store, cfg)
}

fn create_identify_behavior(local_public_key: PublicKey) -> identify::Behaviour {
    identify::Behaviour::new(identify::Config::new(
        "/p2pclipboard/1.0.0".into(),
        local_public_key,
    ))
}

fn create_mdns_behavior(
    local_peer_id: PeerId,
    pre_shared_key: Option<String>,
    disable_mdns: bool,
) -> mdns::Behaviour<mdns::tokio::Tokio> {
    let mut mdns_config = mdns::Config::default();
    let fingerprint = match pre_shared_key {
        Some(psk) => {
            let mut seed_key_bytes =
                SigningKey::from_pkcs8_pem(std::str::from_utf8(&ID_SEED).unwrap())
                    .unwrap()
                    .to_bytes();
            let seed_key = Keypair::ed25519_from_bytes(&mut seed_key_bytes).unwrap();
            Some(Vec::from(
                seed_key
                    .derive_secret(psk.as_ref())
                    .expect("can derive secret for ed25519"),
            ))
        }
        None => None,
    };
    mdns_config.service_fingerprint = fingerprint;
    mdns_config.disabled = disable_mdns;
    mdns::tokio::Behaviour::new(mdns_config, local_peer_id).expect("mdns correct")
}

fn parse_ipv4_with_port(input: Option<String>) -> Result<(Ipv4Addr, u16), &'static str> {
    if let Some(input_str) = input {
        let parts: Vec<&str> = input_str.split(':').collect();

        if parts.len() == 2 {
            let socket_address: Result<SocketAddrV4, _> = input_str.parse();
            match socket_address {
                Ok(socket) => Ok((*socket.ip(), socket.port())),
                Err(_) => Err("Invalid input format"),
            }
        } else if parts.len() == 1 {
            let ip_addr: Result<Ipv4Addr, _> = parts[0].parse();
            match ip_addr {
                Ok(ip) => Ok((ip, 0)),
                _ => Err("Invalid IP address or port number"),
            }
        } else {
            Err("Invalid input format")
        }
    } else {
        Err("Input is None")
    }
}
