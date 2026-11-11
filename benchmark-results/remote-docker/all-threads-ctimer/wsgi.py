import atexit
import json
import os
import resource

from django.core.wsgi import get_wsgi_application

os.environ.setdefault('DJANGO_SETTINGS_MODULE', 'hello_django.settings')


def emit(kind, value):
    os.write(1, (kind + ' ' + json.dumps(value) + '\n').encode())


def cpu_seconds():
    usage = resource.getrusage(resource.RUSAGE_SELF)
    return usage.ru_utime + usage.ru_stime


if os.environ.get('PYROSCOPE_BENCHMARK') == 'true':
    import pyroscope

    name = os.environ['PYROSCOPE_BENCHMARK_IMPL']
    implementation = {
        'py-spy': pyroscope.ProfilerImplementation.PySpy,
        'stack': pyroscope.ProfilerImplementation.Stack,
        'signal': pyroscope.ProfilerImplementation.Signal,
    }[name]
    sample_rate = int(os.environ['PYROSCOPE_BENCHMARK_SAMPLERATE'])
    options = dict(
        application_name='django.app.opt',
        server_address=os.environ['PYROSCOPE_BENCHMARK_SERVER'],
        enable_logging=False,
        sample_rate=sample_rate,
        cpu_implementation=implementation,
        gil_only=False,
        oncpu=True,
        signal_timer=pyroscope.SignalTimer.CTimer,
    )
    emit('BENCH_PROFILER_SETTINGS', dict(
        implementation=name, gil_only=options['gil_only'],
        signal_timer='CTimer', sample_rate=options['sample_rate'], pid=os.getpid(),
    ))
    cpu_before_configure = cpu_seconds()
    if not pyroscope.configure(**options):
        raise RuntimeError('Profiler initialization failed')
    cpu_after_configure = cpu_seconds()

    def stop_profiler():
        cpu_before_shutdown = cpu_seconds()
        stopped = pyroscope.shutdown()
        cpu_after_shutdown = cpu_seconds()
        emit('BENCH_PROFILE_CPU', dict(
            pid=os.getpid(), implementation=name, shutdown_succeeded=stopped,
            cpu_before_configure=cpu_before_configure,
            cpu_after_configure=cpu_after_configure,
            cpu_before_shutdown=cpu_before_shutdown,
            cpu_after_shutdown=cpu_after_shutdown,
        ))

    atexit.register(stop_profiler)

application = get_wsgi_application()
print('BENCH_WORKER_READY', os.getpid(), flush=True)
