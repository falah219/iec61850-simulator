import { io } from 'socket.io-client';

// VITE_BACKEND_URL kosong -> backend dianggap di host yang sama dengan halaman, port 6061
const socketUrl = import.meta.env.VITE_BACKEND_URL || `http://${window.location.hostname}:6061`;

console.log(`Creating socket connection to: ${socketUrl}`);
// Reconnect tanpa batas (default socket.io), supaya indikator kembali "Tersambung" saat backend hidup lagi
const socket = io(socketUrl, { transports: ['websocket', 'polling'] });

export default socket;
