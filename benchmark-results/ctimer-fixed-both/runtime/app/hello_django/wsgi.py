"""
WSGI config for hello_django project.

It exposes the WSGI callable as a module-level variable named ``application``.

For more information on this file, see
https://docs.djangoproject.com/en/3.2/howto/deployment/wsgi/
"""

import os

from django.core.wsgi import get_wsgi_application

os.environ.setdefault('DJANGO_SETTINGS_MODULE', 'hello_django.settings')

if os.environ.get('PYROSCOPE_BENCHMARK') == 'true':
	addr = os.environ["PYROSCOPE_BENCHMARK_SERVER"]
	samplerate = os.environ.get('PYROSCOPE_BENCHMARK_SAMPLERATE')
	if samplerate:
		samplerate = int(samplerate)
	else:
		samplerate = 100
	print(f'using pyroscope with samplerate {samplerate}')
	print(f'running python against {addr}')
	import pyroscope
	import atexit
	implementation = {"py-spy": pyroscope.ProfilerImplementation.PySpy, "stack": pyroscope.ProfilerImplementation.Stack, "signal": pyroscope.ProfilerImplementation.Signal}[os.environ["PYROSCOPE_BENCHMARK_IMPL"]]
	res = pyroscope.configure(
		application_name = "django.app.opt",
		server_address = addr,
		enable_logging = False,
		sample_rate=samplerate,
		cpu_implementation=implementation,
	)
	if not res:
		raise RuntimeError("Profiler initialization failed")
	atexit.register(pyroscope.shutdown)
	
else:
	print(f'not using pyroscope')

application = get_wsgi_application()

print("BENCH_WORKER_READY", os.getpid(), flush=True)
