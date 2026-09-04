from auth import AuthService


def test_login() -> None:
    assert AuthService().login("ada")["token"] == "token-ada"
