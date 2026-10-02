import asyncio


async def wait_for_event(event):
    await event.wait()


async def nested_worker(event):
    await wait_for_event(event)


async def finished_worker():
    return


async def main():
    asyncio.get_running_loop().set_debug(True)
    event = asyncio.Event()
    asyncio.create_task(nested_worker(event), name="nested-worker")
    asyncio.create_task(asyncio.sleep(3600), name="sleep-worker")
    finished_task = asyncio.create_task(finished_worker(), name="finished-worker")
    await finished_task
    print("ready", flush=True)
    await event.wait()


asyncio.run(main())
