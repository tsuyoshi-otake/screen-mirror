package com.screenmirror;

import android.os.Build;

import org.json.JSONObject;

import java.net.DatagramPacket;
import java.net.DatagramSocket;
import java.net.Inet4Address;
import java.net.InetAddress;
import java.net.InetSocketAddress;
import java.net.NetworkInterface;
import java.net.SocketTimeoutException;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.Enumeration;
import java.util.HashSet;
import java.util.List;
import java.util.Set;
import java.util.UUID;
import java.util.concurrent.atomic.AtomicBoolean;

final class DiscoveryAgent {
    static final int DISCOVERY_PORT = 47777;
    static final int DISCOVERY_PROBE_PORT = 47776;
    private static final String PROTOCOL = "screen-mirror.discovery";
    private static final String PROBE_PROTOCOL = "screen-mirror.discovery-probe";
    private static final int VERSION = 1;

    static final class Peer {
        final String instanceId;
        final String deviceName;
        final String role;
        final String host;
        final int streamPort;
        final int audioPort;
        final int displayWidth;
        final int displayHeight;
        final int refreshHz;

        Peer(String instanceId, String deviceName, String role, String host, int streamPort, int audioPort, int displayWidth, int displayHeight, int refreshHz) {
            this.instanceId = instanceId;
            this.deviceName = deviceName;
            this.role = role;
            this.host = host;
            this.streamPort = streamPort;
            this.audioPort = audioPort;
            this.displayWidth = displayWidth;
            this.displayHeight = displayHeight;
            this.refreshHz = refreshHz;
        }

        @Override
        public String toString() {
            return deviceName + " " + host + ":" + streamPort + " (" + role + ")";
        }
    }

    private final String instanceId = Build.MODEL.replace(' ', '-') + "-" + UUID.randomUUID();
    private final AtomicBoolean running = new AtomicBoolean(false);
    private Thread thread;

    void startReceiverBeacon(int streamPort, int audioPort, int displayWidth, int displayHeight, int refreshHz, String pin) {
        stop();
        running.set(true);
        String normalizedPin = Pin.normalize(pin);
        thread = new Thread(() -> runBeacon("receiver", streamPort, audioPort, displayWidth, displayHeight, refreshHz, normalizedPin), "discovery-beacon");
        thread.start();
    }

    void stop() {
        running.set(false);
        if (thread != null) {
            thread.interrupt();
            thread = null;
        }
    }

    List<Peer> discoverReceivers(long timeoutMs, String pin) throws Exception {
        ArrayList<Peer> peers = new ArrayList<>();
        long deadline = System.currentTimeMillis() + timeoutMs;
        byte[] buffer = new byte[2048];
        String expectedPinHash = Pin.hash(pin);

        try (DatagramSocket broadcastSocket = new DatagramSocket(null);
             DatagramSocket probeSocket = new DatagramSocket()) {
            broadcastSocket.setReuseAddress(true);
            broadcastSocket.bind(new InetSocketAddress(DISCOVERY_PORT));
            broadcastSocket.setSoTimeout(100);
            probeSocket.setSoTimeout(100);
            long nextProbeAt = 0;

            while (System.currentTimeMillis() < deadline) {
                long now = System.currentTimeMillis();
                if (now >= nextProbeAt) {
                    sendDiscoveryProbes(probeSocket, pin);
                    nextProbeAt = now + 1000;
                }
                addDiscoveredPeer(peers, receivePeer(broadcastSocket, buffer, expectedPinHash));
                addDiscoveredPeer(peers, receivePeer(probeSocket, buffer, expectedPinHash));
            }
        }

        return peers;
    }

    private void runBeacon(String role, int streamPort, int audioPort, int displayWidth, int displayHeight, int refreshHz, String pin) {
        try (DatagramSocket socket = new DatagramSocket(null)) {
            socket.setReuseAddress(true);
            socket.bind(new InetSocketAddress(DISCOVERY_PROBE_PORT));
            socket.setBroadcast(true);
            socket.setSoTimeout(200);
            byte[] payload = announcement(role, streamPort, audioPort, displayWidth, displayHeight, refreshHz, pin);
            byte[] buffer = new byte[2048];
            long nextBroadcastAt = 0;
            while (running.get()) {
                long now = System.currentTimeMillis();
                if (now >= nextBroadcastAt) {
                    DatagramPacket packet = new DatagramPacket(
                            payload,
                            payload.length,
                            InetAddress.getByName("255.255.255.255"),
                            DISCOVERY_PORT
                    );
                    socket.send(packet);
                    nextBroadcastAt = now + 1000;
                }

                try {
                    DatagramPacket probePacket = new DatagramPacket(buffer, buffer.length);
                    socket.receive(probePacket);
                    if (matchesProbe(probePacket, role, pin)) {
                        socket.send(new DatagramPacket(
                                payload,
                                payload.length,
                                probePacket.getAddress(),
                                probePacket.getPort()
                        ));
                    }
                } catch (SocketTimeoutException ignored) {
                } catch (Exception ignored) {
                }
            }
        } catch (Exception ignored) {
        }
    }

    private void sendDiscoveryProbes(DatagramSocket socket, String pin) throws Exception {
        JSONObject json = new JSONObject();
        json.put("protocol", PROBE_PROTOCOL);
        json.put("version", VERSION);
        json.put("wanted_role", "receiver");
        json.put("pin_hash", Pin.hash(pin));
        json.put("timestamp_ms", System.currentTimeMillis());
        byte[] payload = json.toString().getBytes(StandardCharsets.UTF_8);

        for (InetAddress address : unicastProbeAddresses()) {
            try {
                socket.send(new DatagramPacket(
                        payload,
                        payload.length,
                        address,
                        DISCOVERY_PROBE_PORT
                ));
            } catch (Exception ignored) {
            }
        }
    }

    private static List<InetAddress> unicastProbeAddresses() throws Exception {
        ArrayList<InetAddress> addresses = new ArrayList<>();
        Set<String> seen = new HashSet<>();
        Enumeration<NetworkInterface> interfaces = NetworkInterface.getNetworkInterfaces();
        while (interfaces.hasMoreElements() && addresses.size() < 2048) {
            NetworkInterface network = interfaces.nextElement();
            if (!network.isUp() || network.isLoopback()) {
                continue;
            }
            Enumeration<InetAddress> interfaceAddresses = network.getInetAddresses();
            while (interfaceAddresses.hasMoreElements() && addresses.size() < 2048) {
                InetAddress ownAddress = interfaceAddresses.nextElement();
                if (!(ownAddress instanceof Inet4Address) || ownAddress.isLoopbackAddress() || ownAddress.isLinkLocalAddress()) {
                    continue;
                }
                byte[] octets = ownAddress.getAddress();
                int first = octets[0] & 0xff;
                int second = octets[1] & 0xff;
                boolean privateLan = first == 10
                        || (first == 172 && second >= 16 && second <= 31)
                        || (first == 192 && second == 168)
                        || (first == 100 && second >= 64 && second <= 127);
                if (!privateLan) {
                    continue;
                }
                int ownLast = octets[3] & 0xff;
                for (int last = 1; last < 255 && addresses.size() < 2048; last++) {
                    if (last == ownLast) {
                        continue;
                    }
                    byte[] candidate = octets.clone();
                    candidate[3] = (byte) last;
                    InetAddress address = InetAddress.getByAddress(candidate);
                    if (seen.add(address.getHostAddress())) {
                        addresses.add(address);
                    }
                }
            }
        }
        return addresses;
    }

    private static boolean matchesProbe(DatagramPacket packet, String role, String pin) throws Exception {
        String text = new String(packet.getData(), packet.getOffset(), packet.getLength(), StandardCharsets.UTF_8);
        JSONObject json = new JSONObject(text);
        if (!PROBE_PROTOCOL.equals(json.optString("protocol")) || json.optInt("version") != VERSION) {
            return false;
        }
        String wantedRole = json.optString("wanted_role", "");
        if (!wantedRole.isEmpty() && !role.equals(wantedRole)) {
            return false;
        }
        String wantedPinHash = json.optString("pin_hash", "");
        return wantedPinHash.isEmpty() || wantedPinHash.equals(Pin.hash(pin));
    }

    private static Peer receivePeer(DatagramSocket socket, byte[] buffer, String expectedPinHash) {
        try {
            DatagramPacket packet = new DatagramPacket(buffer, buffer.length);
            socket.receive(packet);
            return decodePeer(packet, expectedPinHash);
        } catch (SocketTimeoutException ignored) {
            return null;
        } catch (Exception ignored) {
            return null;
        }
    }

    private static void addDiscoveredPeer(List<Peer> peers, Peer peer) {
        if (peer == null || !"receiver".equals(peer.role)) {
            return;
        }
        for (Peer existing : peers) {
            if (existing.instanceId.equals(peer.instanceId)) {
                return;
            }
        }
        peers.add(peer);
    }

    private byte[] announcement(String role, int streamPort, int audioPort, int displayWidth, int displayHeight, int refreshHz, String pin) throws Exception {
        JSONObject json = new JSONObject();
        json.put("protocol", PROTOCOL);
        json.put("version", VERSION);
        json.put("instance_id", instanceId);
        json.put("device_name", Build.MODEL);
        json.put("role", role);
        json.put("stream_port", streamPort);
        json.put("audio_port", audioPort);
        json.put("pin_hash", Pin.hash(pin));
        JSONObject display = new JSONObject();
        display.put("width", displayWidth);
        display.put("height", displayHeight);
        if (refreshHz > 0) {
            display.put("refresh_hz", refreshHz);
        }
        json.put("display", display);
        json.put("timestamp_ms", System.currentTimeMillis());
        return json.toString().getBytes(StandardCharsets.UTF_8);
    }

    private static Peer decodePeer(DatagramPacket packet, String expectedPinHash) throws Exception {
        String text = new String(packet.getData(), packet.getOffset(), packet.getLength(), StandardCharsets.UTF_8);
        JSONObject json = new JSONObject(text);
        if (!PROTOCOL.equals(json.optString("protocol")) || json.optInt("version") != VERSION) {
            throw new IllegalArgumentException("unsupported discovery packet");
        }
        if (!expectedPinHash.equals(json.optString("pin_hash", ""))) {
            throw new IllegalArgumentException("PIN mismatch");
        }
        InetAddress address = packet.getAddress();
        if (!(address instanceof Inet4Address)) {
            throw new IllegalArgumentException("only IPv4 discovery is supported");
        }
        JSONObject display = json.optJSONObject("display");
        int displayWidth = display == null ? 0 : display.optInt("width", 0);
        int displayHeight = display == null ? 0 : display.optInt("height", 0);
        int refreshHz = display == null ? 0 : display.optInt("refresh_hz", 0);
        int streamPort = json.getInt("stream_port");

        return new Peer(
                json.getString("instance_id"),
                json.optString("device_name", "Android"),
                json.getString("role"),
                address.getHostAddress(),
                streamPort,
                json.optInt("audio_port", streamPort + 1),
                displayWidth,
                displayHeight,
                refreshHz
        );
    }
}
