package com.screenmirror;

import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;

final class Pin {
    static final String DEFAULT = "0000";

    private Pin() {
    }

    static String normalize(String pin) {
        String value = pin == null ? "" : pin.trim();
        if (value.length() != 4) {
            throw new IllegalArgumentException("PIN must be exactly four digits");
        }
        for (int index = 0; index < value.length(); index++) {
            char digit = value.charAt(index);
            if (digit < '0' || digit > '9') {
                throw new IllegalArgumentException("PIN must be exactly four digits");
            }
        }
        return value;
    }

    static String hash(String pin) throws Exception {
        MessageDigest digest = MessageDigest.getInstance("SHA-256");
        digest.update("screen-mirror.pin.v1:".getBytes(StandardCharsets.UTF_8));
        byte[] bytes = digest.digest(normalize(pin).getBytes(StandardCharsets.UTF_8));
        StringBuilder builder = new StringBuilder(bytes.length * 2);
        for (byte value : bytes) {
            builder.append(String.format("%02x", value & 0xff));
        }
        return builder.toString();
    }
}
