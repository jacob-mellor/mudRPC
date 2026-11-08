<?php
/* The native server over UDP, in its own process: `php server.php <bind port> <client port>`; it answers until stdin closes. */
declare(strict_types=1);

require __DIR__ . '/vendor/autoload.php';
require __DIR__ . '/mud/Mud.php';
require __DIR__ . '/native.php';

$udp = new Mud\Udp("127.0.0.1:$argv[1]", "127.0.0.1:$argv[2]");
$native = new Native(new Mud\Peer('drone', implode(array_map('chr', range(1, 32))), 2, 1));
$flush = function () use ($native, $udp): void {
    $native->pump();
    foreach ($native->out as $o) {
        $udp->send($o);
    }
    $native->out = [];
};
$flush();   // its listens reach the socket before a client is told to call
stream_set_blocking(STDIN, false);
echo "ready\n";
while (!feof(STDIN)) {
    while (($w = $udp->poll()) !== null) {
        $native->take($w[0], $w[1]);
    }
    $flush();
    fgets(STDIN);
    usleep(1000);
}
