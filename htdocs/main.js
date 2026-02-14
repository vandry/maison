const {Climate} = require('codegen/maison_pb.js');
const {MaisonClient} = require('codegen/maison_grpc_web_pb.js');

class Maison {
    constructor (api) {
        this.api = api;
    }

    run = () => {
        this.run_livetemp();
    }

    run_livetemp = () => {
        var top = this;
        var req = new proto.google.protobuf.Empty();
        var stream = this.api.monitorLiveTemperatures(req, {});
        stream.on('data', function(response) {
            var els = document.getElementsByClassName("climate-" + response.getUnit());
            for (var i = 0; i < els.length; i++) {
                var livetemp_els = els[i].getElementsByClassName("livetemp");
                for (var j = 0; j < livetemp_els.length; j++) {
                    livetemp_els[j].textContent = response.getTemperature().toFixed(1) + " \xb0C";
                }
            }
        });
        stream.on('status', function(status) {
            setTimeout(() => { top.run_livetemp(); }, 1000);
        });
        stream.on('end', function() {
            top.run_livetemp();
        });
    }
}

new Maison(new MaisonClient(URL.parse(window.location).toString())).run();
