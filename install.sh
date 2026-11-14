#!/bin/bash
# Build image, push ke registry, lalu pasang di Kubernetes (pola sama dengan iec104_rtu_simulator).
#
# Pakai:
#   ./install.sh <image-tag> <http-port> <http-nodeport> <ui-nodeport> <backend-host> <mms-port-awal> [jumlah-port-mms=100] [ied-count=1]
# Contoh:
#   ./install.sh 1 6061 30061 30041 192.168.1.10 30102 100 20
#     UI      : http://192.168.1.10:30041
#     API     : http://192.168.1.10:30061 (Socket.IO juga di sini)
#     IED ke-1: 192.168.1.10:30102, IED ke-n: 30102 + n - 1 (sampai 30201)
#
# Port MMS di dalam container dibuat sama dengan NodePort-nya (MMS_PORT / MMS_PORT_BASE), jadi port
# yang tampil di UI = port yang dipakai FEP. Semua NodePort harus di 30000-32767.
# ied-count hanya dipakai saat pertama kali (belum ada file autosave di volume).
#
# Bisa diubah lewat env: REGISTRY (grita.id/scada), NAMESPACE (scada-grita), PULL_SECRET (harbor-secret),
# UI_PORT (4061).

set -euo pipefail

if [ $# -lt 6 ]; then
  sed -n '2,19p' "$0"
  exit 1
fi

IMAGE_TAG=$1
HTTP_PORT=$2
HTTP_NODEPORT=$3
UI_NODEPORT=$4
BACKEND_HOST=$5
MMS_PORT=$6
MMS_COUNT=${7:-100}
IED_COUNT=${8:-1}
REGISTRY=${REGISTRY:-grita.id/scada}
NAMESPACE=${NAMESPACE:-scada-grita}
PULL_SECRET=${PULL_SECRET:-harbor-secret}
UI_PORT=${UI_PORT:-4061}
MMS_PORT_BASE=$((MMS_PORT + 1))
MMS_LAST=$((MMS_PORT + MMS_COUNT - 1))

in_range() { [ "$1" -ge 30000 ] && [ "$1" -le 32767 ]; }
for p in "$HTTP_NODEPORT" "$UI_NODEPORT" "$MMS_PORT" "$MMS_LAST"; do
  in_range "$p" || { echo "NodePort $p di luar 30000-32767"; exit 1; }
done
for p in "$HTTP_NODEPORT" "$UI_NODEPORT"; do
  if [ "$p" -ge "$MMS_PORT" ] && [ "$p" -le "$MMS_LAST" ]; then
    echo "NodePort $p bentrok dengan rentang MMS $MMS_PORT-$MMS_LAST"
    exit 1
  fi
done
if [ "$IED_COUNT" -gt "$MMS_COUNT" ]; then
  echo "ied-count ($IED_COUNT) lebih dari jumlah port MMS ($MMS_COUNT)"
  exit 1
fi

BACKEND_IMAGE=iec61850-simulator-backend-$IMAGE_TAG
FRONTEND_IMAGE=iec61850-simulator-frontend-$IMAGE_TAG

echo "Build image..."
docker build -t "$BACKEND_IMAGE" -f backend/Dockerfile.prod backend
docker build -t "$FRONTEND_IMAGE" -f frontend/Dockerfile.prod \
  --build-arg VITE_BACKEND_URL="http://$BACKEND_HOST:$HTTP_NODEPORT" \
  --build-arg UI_PORT="$UI_PORT" frontend

echo "Tag & push ke $REGISTRY..."
docker tag "$BACKEND_IMAGE" "$REGISTRY/$BACKEND_IMAGE"
docker tag "$FRONTEND_IMAGE" "$REGISTRY/$FRONTEND_IMAGE"
docker push "$REGISTRY/$BACKEND_IMAGE"
docker push "$REGISTRY/$FRONTEND_IMAGE"

# Daftar port MMS untuk Service (port = targetPort = nodePort)
MMS_SERVICE_PORTS=""
for ((p = MMS_PORT; p <= MMS_LAST; p++)); do
  MMS_SERVICE_PORTS+="    - name: mms-$p
      port: $p
      targetPort: $p
      nodePort: $p
      protocol: TCP
"
done

export IMAGE_TAG HTTP_PORT HTTP_NODEPORT UI_PORT UI_NODEPORT MMS_PORT MMS_PORT_BASE IED_COUNT \
  REGISTRY NAMESPACE PULL_SECRET MMS_SERVICE_PORTS

# hanya variabel ini yang diganti (teks lain berbentuk $... di manifest dibiarkan)
# shellcheck disable=SC2016
VARS='${IMAGE_TAG} ${HTTP_PORT} ${HTTP_NODEPORT} ${UI_PORT} ${UI_NODEPORT} ${MMS_PORT} ${MMS_PORT_BASE} ${IED_COUNT} ${REGISTRY} ${NAMESPACE} ${PULL_SECRET} ${MMS_SERVICE_PORTS}'

echo "Pasang di Kubernetes (namespace $NAMESPACE)..."
for f in backend/k8s/configmap.yaml backend/k8s/deployment.yaml frontend/k8s/deployment.yaml; do
  envsubst "$VARS" < "$f" | kubectl apply -f -
done

echo "Selesai."
echo "  UI      : http://$BACKEND_HOST:$UI_NODEPORT"
echo "  API     : http://$BACKEND_HOST:$HTTP_NODEPORT/health"
echo "  MMS     : $BACKEND_HOST:$MMS_PORT (IED ke-1) .. $MMS_LAST"
