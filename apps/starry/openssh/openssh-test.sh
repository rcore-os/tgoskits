#!/bin/sh

apk add openssh

echo "=== generate host key ==="
ssh-keygen -t ed25519 -f /etc/ssh/ssh_host_ed25519_key -N ""

echo "=== generate client key ==="
ssh-keygen -t ed25519 -f /root/.ssh/id_ed25519 -N ""
cat /root/.ssh/id_ed25519.pub >> /root/.ssh/authorized_keys
chmod 600 /root/.ssh/authorized_keys

echo "=== configure sshd ==="
sed -i 's/#PermitRootLogin.*/PermitRootLogin yes/' /etc/ssh/sshd_config
sed -i 's/#PubkeyAuthentication.*/PubkeyAuthentication yes/' /etc/ssh/sshd_config

echo "=== start sshd ==="
mkdir -p /var/run
/usr/sbin/sshd -D -e &
SSHD_PID=$!
sleep 2

echo "=== ssh localhost ==="
RESULT=$(ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o BatchMode=yes root@127.0.0.1 "echo SSH_CONNECTED" 2>&1)
echo "$RESULT"

kill $SSHD_PID 2>/dev/null

if echo "$RESULT" | grep -q "SSH_CONNECTED"; then
    echo "OPENSSH_TEST_PASSED"
else
    echo "OPENSSH_TEST_FAILED"
fi
