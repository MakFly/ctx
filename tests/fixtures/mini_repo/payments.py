from db import save_payment


def charge(user_id: int, amount: int) -> bool:
    return save_payment(user_id, amount)


def retry_payment(user_id: int, amount: int) -> bool:
    return charge(user_id, amount)
