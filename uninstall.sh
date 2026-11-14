#!/bin/bash
# Hapus deployment, service, configmap, dan image (pola sama dengan iec104_rtu_simulator).
#
# Pakai: ./uninstall.sh <image-tag> [--hapus-data]
#   --hapus-data: hapus juga volume autosave (konfigurasi semua IED hilang)
#
# Env yang sama dengan install.sh: REGISTRY, NAMESPACE.

set -uo pipefail

if [ $# -lt 1 ]; then
  sed -n '2,7p' "$0"
  exit 1
fi

IMAGE_TAG=$1
REGISTRY=${REGISTRY:-grita.id/scada}
NAMESPACE=${NAMESPACE:-scada-grita}
BACKEND=iec61850-simulator-backend-$IMAGE_TAG
FRONTEND=iec61850-simulator-frontend-$IMAGE_TAG

echo "Hapus resource Kubernetes (namespace $NAMESPACE)..."
kubectl -n "$NAMESPACE" delete deployment "$FRONTEND" "$BACKEND" --ignore-not-found
kubectl -n "$NAMESPACE" delete service "$FRONTEND-service" "$BACKEND-service" --ignore-not-found
kubectl -n "$NAMESPACE" delete configmap "$BACKEND" --ignore-not-found
if [ "${2:-}" = "--hapus-data" ]; then
  kubectl -n "$NAMESPACE" delete pvc "iec61850-simulator-data-$IMAGE_TAG" --ignore-not-found
else
  echo "Volume autosave iec61850-simulator-data-$IMAGE_TAG dibiarkan (pakai --hapus-data untuk menghapus)."
fi

echo "Hapus image lokal..."
docker rmi "$REGISTRY/$BACKEND" "$REGISTRY/$FRONTEND" "$BACKEND" "$FRONTEND" 2>/dev/null || true

echo "Selesai."
