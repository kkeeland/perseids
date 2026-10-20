# ruby test/runner_test.rb — the runner's logic, on your machine (no box, no Docker).
require "minitest/autorun"
require "tmpdir"
require "fileutils"
require "yaml"
require_relative "../lib/runner"

class RunnerTest < Minitest::Test
  def ev(src, ctx = {}, failed = false)
    Perseids::Expr.new(ctx, failed).eval(src)
  end

  def test_expressions
    ctx = { "matrix" => { "node" => 20 }, "env" => { "A" => "x" }, "github" => { "event_name" => "push" } }
    assert_equal 20, ev("matrix.node", ctx)
    assert_equal true, ev("github.event_name == 'PUSH'", ctx) # strings compare case-insensitively
    assert_equal "fallback", ev("env.MISSING || 'fallback'", ctx)
    assert_equal "ubuntu-latest", ev("(github.event_name == 'workflow_dispatch' && vars.R) || 'ubuntu-latest'", ctx)
    assert_equal true, ev("matrix.node >= 18 && !contains(env.A, 'y')", ctx)
    assert_equal "v-20", ev("format('v-{0}', matrix.node)", ctx)
    assert_equal true, ev("startsWith(github['event_name'], 'pu')", ctx)
    assert_equal "it's", ev("'it''s'")
    assert_equal "a b", Perseids.interpolate("${{ env.A == 'x' && 'a' }} b", ctx)
    assert_raises(Perseids::Unsupported) { ev("github.event.commits.*.message", ctx) }
    assert_raises(Perseids::Unsupported) { ev("nope()", ctx) }
  end

  def test_if_implies_success
    assert Perseids.cond(nil, {}, false)
    refute Perseids.cond(nil, {}, true)
    refute Perseids.cond("true", {}, true)
    assert Perseids.cond("${{ always() }}", {}, true)
    assert Perseids.cond("failure()", {}, true)
  end

  def test_if_that_reads_secrets_or_vars_cant_be_decided
    assert_raises(Perseids::Unsupported) { Perseids.cond("secrets.TOKEN != ''", {}, false) }
    assert_raises(Perseids::Unsupported) { Perseids.cond("vars.DEPLOY == 'true'", {}, false) }
    # a side that can't change the value doesn't count
    refute Perseids.cond("github.event_name == 'release' && secrets.TOKEN != ''", { "github" => { "event_name" => "push" } }, false)
    assert Perseids.cond("always() || vars.X", {}, true)
  end

  def test_matrix_include_exclude
    job = { "strategy" => { "matrix" => {
      "os" => %w[a b], "node" => [18, 20],
      "exclude" => [{ "os" => "b", "node" => 18 }],
      "include" => [{ "node" => 20, "extra" => "yes" }, { "os" => "c", "node" => 22 }],
    } } }
    assert_equal [
      { "os" => "a", "node" => 18 },
      { "os" => "a", "node" => 20, "extra" => "yes" },
      { "os" => "b", "node" => 20, "extra" => "yes" },
      { "os" => "c", "node" => 22 },
    ], Perseids.matrix(job)
    assert_equal [{}], Perseids.matrix({})
  end

  def test_plan_waves_and_unsupported
    wf = YAML.safe_load(<<~Y)
      jobs:
        build: { runs-on: ubuntu-latest, steps: [] }
        test:
          needs: build
          runs-on: ${{ matrix.os }}
          strategy: { matrix: { os: [ubuntu-latest, macos-14] } }
          steps: []
        deploy: { needs: [test], if: "github.event_name == 'release'", runs-on: ubuntu-latest, steps: [] }
    Y
    rows = Perseids.plan(wf, [])
    # a job that needs others is decided when they finish ("gate"), not from an empty needs context now
    assert_equal [[0, "build", 0, "run", ""], [1, "test", 0, "gate", "build"], [1, "test", 1, "gate", "build"],
                  [2, "deploy", 0, "gate", "test"]], rows.map { |r| r[0, 5] }
    assert_equal "test (macos-14)", rows[2][5]
    # asked-for jobs bring the jobs they need
    assert_equal [[0, "build"], [1, "test"], [1, "test"]], Perseids.plan(wf, ["test"]).map { |r| r[0, 2] }

    passed = { "build" => { "result" => "success", "outputs" => {} } }
    assert_equal ["run", ""], Perseids.decide(wf["jobs"]["test"], { "matrix" => { "os" => "ubuntu-latest" } }, passed)
    assert_equal ["unsupported", "it runs on macos-14"], Perseids.decide(wf["jobs"]["test"], { "matrix" => { "os" => "macos-14" } }, passed)
    assert_equal ["skip", "its if: is false"],
                 Perseids.decide(wf["jobs"]["deploy"], { "github" => { "event_name" => "push" } }, "test" => { "result" => "success" })
  end

  # The CLI's half: decide reads each needed entry's state and outputs from the work folder.
  def test_decide_with_real_needs
    Dir.mktmpdir do |d|
      File.write("#{d}/wf.yml", <<~'Y')
        jobs:
          changes: { runs-on: ubuntu-latest, outputs: { code: "${{ steps.f.outputs.code }}" }, steps: [] }
          changes-2: { runs-on: ubuntu-latest, steps: [] }
          test: { needs: changes, if: "needs.changes.outputs.code == 'true'", runs-on: ubuntu-latest, steps: [] }
          after: { needs: test, runs-on: ubuntu-latest, steps: [] }
          gate: { needs: [changes, test], if: always(), runs-on: ubuntu-latest, steps: [] }
          on-fail: { needs: test, if: failure(), runs-on: ubuntu-latest, steps: [] }
          bad: { needs: changes, if: "fromJSON(needs.changes.outputs.none)", runs-on: ubuntu-latest, steps: [] }
      Y
      entry = lambda do |id, state, outputs = nil|
        FileUtils.mkdir_p("#{d}/#{id}")
        File.write("#{d}/#{id}/state", "#{state}\n")
        File.write("#{d}/#{id}/outputs", outputs.to_json) if outputs
      end
      decide = lambda do |job|
        FileUtils.mkdir_p("#{d}/#{job}-0")
        IO.popen(["ruby", File.expand_path("../lib/runner.rb", __dir__), "decide", "#{d}/wf.yml", job, "0", d], &:read).strip
      end

      entry.call("changes-0", "pass", "code" => "true")
      entry.call("changes-2-0", "fail exit 1") # a job whose name starts with changes- isn't one of its entries
      assert_equal "run", decide.call("test")
      assert_equal({ "changes" => { "result" => "success", "outputs" => { "code" => "true" } } },
                   JSON.parse(File.read("#{d}/test-0/needs.json"))) # what the box gets as its needs context

      entry.call("changes-0", "pass", "code" => "false")
      assert_equal "skip its if: is false", decide.call("test")

      entry.call("test-0", "fail exit 1")
      assert_equal "skip needs test", decide.call("after")
      assert_equal "run", decide.call("gate")
      assert_equal "failure", JSON.parse(File.read("#{d}/gate-0/needs.json"))["test"]["result"]
      assert_equal "run", decide.call("on-fail")

      entry.call("test-0", "skip its if: is false")
      assert_equal "skip needs test", decide.call("after")
      assert_equal "skip its if: is false", decide.call("on-fail") # skipped isn't failed

      assert_match(/\Aunsupported /, decide.call("bad")) # an if: that blows up is never a quiet skip
    end
  end

  def test_outputs_and_needs_on_the_box
    Dir.mktmpdir do |d|
      File.write("#{d}/wf.yml", <<~'Y')
        jobs:
          j:
            runs-on: ubuntu-latest
            outputs:
              code: ${{ steps.f.outputs.code }}
              from-needs: ${{ needs.up.outputs.v }}-${{ needs.up.result }}
            steps:
              - id: f
                run: echo "code=true" >> "$GITHUB_OUTPUT"
              - if: secrets.TOKEN != ''
                run: echo publish
      Y
      env = { "PERSEIDS_NEEDS" => { "up" => { "result" => "success", "outputs" => { "v" => "7" } } }.to_json }
      out = Dir.chdir(d) { IO.popen(env, ["ruby", File.expand_path("../lib/runner.rb", __dir__), "run", "wf.yml", "j"], err: %i[child out], &:read) }
      assert_equal 2, $?.exitstatus, out # the secrets-gated step can't be decided on a box: incomplete, not green
      assert_match(/SKIPPED \(its if: reads secrets/, out)
      assert_equal({ "code" => "true", "from-needs" => "7-success" }, JSON.parse(out[/^PERSEIDS_OUTPUTS (.*)$/, 1]))
    end
  end

  def test_composite_action
    Dir.mktmpdir do |d|
      FileUtils.mkdir_p("#{d}/.github/actions/greet")
      File.write("#{d}/.github/actions/greet/action.yml", <<~'Y')
        inputs:
          who: { default: world }
          loud: { default: "no" }
        runs:
          using: composite
          steps:
            - run: echo "hello ${{ inputs.who }}" > out.txt
              shell: bash
            - run: test "${{ inputs.loud }}" = "no"
              shell: bash
      Y
      File.write("#{d}/wf.yml", <<~'Y')
        jobs:
          j:
            runs-on: ubuntu-latest
            steps:
              - uses: ./.github/actions/greet
                with: { who: perseids }
              - run: grep -q "hello perseids" out.txt
              - uses: ./.github/actions/greet
                with: { loud: "yes" }
              - run: echo never
      Y
      out = Dir.chdir(d) { IO.popen(["ruby", File.expand_path("../lib/runner.rb", __dir__), "run", "wf.yml", "j"], err: %i[child out], &:read) }
      assert_equal 1, $?.exitstatus, out
      assert_match(/greet.*ok \(composite\)/, out)
      assert_match(/\[ ?\d\] grep -q "hello perseids" out.txt\s+ok/, out)
      assert_match(/skipped \(an earlier step failed\)/, out)
    end
  end

  def test_run_a_job
    Dir.mktmpdir do |d|
      File.write("#{d}/wf.yml", <<~'Y')
        env: { GREETING: hi }
        jobs:
          j:
            runs-on: ubuntu-latest
            steps:
              - uses: actions/checkout@v4
              - id: a
                run: |
                  echo "FROM_STEP=one" >> "$GITHUB_ENV"
                  echo "out=two" >> "$GITHUB_OUTPUT"
                  printf 'ML<<EOF\nl1\nl2\nEOF\n' >> "$GITHUB_ENV"
              - run: test "$FROM_STEP-${{ steps.a.outputs.out }}-$GREETING" = "one-two-hi" && test "$(printf '%s' "$ML" | wc -l | tr -d ' ')" = 1
              - run: false | true # no pipefail without an explicit shell, as on GitHub
              - run: exit 3
                continue-on-error: true
              - uses: some/unknown-action@v1
              - run: exit 1
              - run: echo never
              - if: always()
                run: echo cleanup
      Y
      out = Dir.chdir(d) do
        IO.popen(["ruby", File.expand_path("../lib/runner.rb", __dir__), "run", "wf.yml", "j"], err: %i[child out], &:read)
      end
      assert_equal 1, $?.exitstatus, out
      assert_match(/\[ 2\] .* ok /, out)
      assert_match(/\[ 3\] .* ok /, out)
      assert_match(/failed \(continue-on-error\)/, out)
      assert_match(/SKIPPED \(some\/unknown-action@v1/, out)
      assert_match(/skipped \(an earlier step failed\)/, out)
      assert_match(/cleanup/, out)
      assert_match(/PERSEIDS_RESULT fail\texit 1/, out)
    end
  end
end
