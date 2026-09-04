import json

from click.testing import CliRunner

from ctx.cli import app


def test_cli_integration(mini_repo):
    runner = CliRunner()
    indexed = runner.invoke(app, ["index", str(mini_repo)])
    assert indexed.exit_code == 0, indexed.output
    searched = runner.invoke(app, ["search", "login", "--json"])
    assert searched.exit_code == 0, searched.output
    assert json.loads(searched.stdout)["hits"][0]["path"] == "auth.py"
