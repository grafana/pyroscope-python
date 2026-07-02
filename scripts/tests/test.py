import hashlib
import os
import signal
import threading
import logging
import time
import traceback
import argparse
import multiprocessing

import pyroscope
import threading

import uuid
from urllib.parse import quote

try:
    from urllib.request import Request, urlopen
except ImportError:
    from urllib2 import Request, urlopen


app_name = 'pyroscopers.python.test'
logger = logging.getLogger()

event = threading.Event()


def hash(string):
    string = string.encode()
    string = hashlib.sha256(string).hexdigest()

    return string


def multihash(string):
    while not event.is_set():
        time.sleep(0.2)
        e = time.time() + 0.1
        while time.time() < e:
            string = hash(string)
    return string


def multihash2(string):
    while not event.is_set():
        time.sleep(0.2)
        e = time.time() + 0.1
        while time.time() < e:
            string = hash(string)
    return string


def wait_render(canary):
    while True:
        time.sleep(2)
        query = f'process_cpu:cpu:nanoseconds:cpu:nanoseconds{{service_name="pyroscopers.python.test", canary="{canary}"}}'
        u = 'http://localhost:4040/pyroscope/render?from=now-1h&until=now&&query=' + quote(query)
        response = None
        try:
            logging.info('render %s', u)
            req = Request(u)
            response = urlopen(req)
            code = response.getcode()
            body = response.read()
            logging.info("render body %s", body.decode('utf-8'))
            if code == 200 and body != b'' and b'multihash' in body:
                print(f'good {canary}')
                return
        except Exception:
            if response is not None:
                response.close()
            traceback.print_exc()
            continue


def do_one_test(on_cpu, gil_only):
    logging.info("do_one_test on_cpu=%s gil_only=%s", on_cpu, gil_only)
    canary = uuid.uuid4().hex
    logging.info('canary %s', canary)
    pyroscope.configure(
        application_name=app_name,
        server_address="http://localhost:4040",
        enable_logging=True,
        oncpu=on_cpu,
        gil_only=gil_only,
        report_pid=True,
        report_thread_id=True,
        report_thread_name=True,

        tags={
            "oncpu": '{}'.format(on_cpu),
            "gil_only": '{}'.format(gil_only),
            "canary": canary,
        }
    )

    thread_1 = threading.Thread(target=multihash, args=('abc',))
    thread_2 = threading.Thread(target=multihash2, args=('abc',))
    thread_1.start()
    thread_2.start()

    def watchdog():
        logging.info('Watchdog expired. Test timeout. Exiting...')
        exit(7)

    alarm = threading.Timer(120, watchdog)
    alarm.start()

    wait_render(canary)

    alarm.cancel()

    pyroscope.shutdown()

    logging.info("done")

    event.set()
    thread_1.join()
    thread_2.join()


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument(
        '--multiprocessing',
        action=argparse.BooleanOptionalAction,
        default=True,
        help='run all on_cpu/gil_only combinations in parallel (default: true)',
    )
    parser.add_argument(
        '--on-cpu',
        action=argparse.BooleanOptionalAction,
        default=True,
        help='enable on-CPU profiling (single-test mode only)',
    )
    parser.add_argument(
        '--gil-only',
        action=argparse.BooleanOptionalAction,
        default=True,
        help='profile GIL-only (single-test mode only)',
    )
    parser.add_argument(
        '--log-level',
        default='INFO',
        choices=['DEBUG', 'INFO', 'WARNING', 'ERROR', 'CRITICAL'],
        help='logging level (default: INFO)',
    )
    args = parser.parse_args()

    log_level = getattr(logging, args.log_level)
    logger.setLevel(log_level)
    multiprocessing.log_to_stderr(log_level)
    if args.multiprocessing:
        procs = []
        res = []
        for on_cpu in [True, False]:
            for gil_only in [True, False]:
                p = multiprocessing.Process(target=do_one_test, args=(on_cpu, gil_only))
                p.start()
                procs.append((p, "{} {}".format(on_cpu, gil_only)))
        for p in procs:
            p[0].join()
            res.append((p[0].exitcode, p[1]))
        for t in res:
            logging.info("%s", str(t))
        for t in res:
            if t[0] != 0:
                logging.info("test failed %s", str(t))
                exit(1)
    else:
        do_one_test(args.on_cpu, args.gil_only)
