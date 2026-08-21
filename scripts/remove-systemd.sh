#!/bin/bash
set -e

echo "=== Removing Athena Pi Service ==="

# 1. Stop and disable the service if it exists
if systemctl list-unit-files 2>/dev/null | grep -q '^athena.service'; then
    echo "Stopping athena service..."
    sudo systemctl stop athena || true
    echo "Disabling athena service..."
    sudo systemctl disable athena || true
fi

# 2. Remove the systemd unit file and drop-in overrides
if [ -f /etc/systemd/system/athena.service ] || [ -d /etc/systemd/system/athena.service.d ]; then
    echo "Removing systemd unit file and drop-in overrides..."
    sudo rm -f /etc/systemd/system/athena.service
    sudo rm -rf /etc/systemd/system/athena.service.d
    sudo systemctl daemon-reload
fi

# 3. Remove the binary
if [ -f /usr/local/bin/athena ]; then
    echo "Removing binary /usr/local/bin/athena..."
    sudo rm -f /usr/local/bin/athena
fi

# 4. Remove the configuration directory
if [ -d /etc/athena ]; then
    echo "Removing configuration directory /etc/athena..."
    sudo rm -rf /etc/athena
fi

# 5. Remove the service data directory (writable yt-dlp copy)
if [ -d /var/lib/athena ]; then
    echo "Removing service data directory /var/lib/athena..."
    sudo rm -rf /var/lib/athena
fi

# 6. Remove the athena system user and group
if getent passwd athena >/dev/null; then
    echo "Removing athena system user..."
    sudo userdel athena || true
fi
if getent group athena >/dev/null; then
    echo "Removing athena group..."
    sudo groupdel athena || true
fi

# 7. Remove download directory
if [ -d /tmp/athena-downloads ]; then
    echo "Removing download directory /tmp/athena-downloads..."
    sudo rm -rf /tmp/athena-downloads
fi

echo "=== Athena Pi Service Removed Successfully ==="
